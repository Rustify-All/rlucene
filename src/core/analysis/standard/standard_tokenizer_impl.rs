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
use crate::core::analysis::reader::{Reader, ReaderEnum};
use crate::core::analysis::standard::standard_tokenizer::{
  ALPHANUM, EMOJI, HANGUL, HIRAGANA, IDEOGRAPHIC, KATAKANA, NUM, SOUTHEAST_ASIAN,
};
use crate::core::util::attribute_source::AttributeSource;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use std::io::Error;
use std::sync::LazyLock;

static ZZ_CMAP_TOP: LazyLock<Vec<i32>> = LazyLock::new(zz_unpack_cmap_top);

const ZZ_CMAP_TOP_PACKED_0: &str = concat!(
  "\u{1}\0\u{1}\u{100}\u{1}\u{200}\u{1}\u{300}\u{1}\u{400}\u{1}\u{500}\u{1}\u{600}\u{1}\u{700}\u{1}\u{800}\u{1}\u{900}\u{1}\u{a00}\u{1}\u{b00}\u{1}\u{c00}\u{1}\u{d00}",
  "\u{1}\u{e00}\u{1}\u{f00}\u{1}\u{1000}\u{1}\u{1100}\u{1}\u{1200}\u{1}\u{1300}\u{1}\u{1400}\u{1}\u{100}\u{1}\u{1500}\u{1}\u{1600}\u{1}\u{1700}\u{1}\u{1800}\u{1}\u{1900}\u{1}\u{1a00}",
  "\u{1}\u{1b00}\u{1}\u{1c00}\u{1}\u{100}\u{1}\u{1d00}\u{1}\u{1e00}\u{1}\u{1f00}\u{1}\u{2000}\u{1}\u{2100}\u{1}\u{2200}\u{1}\u{2300}\u{1}\u{2400}\u{1}\u{2500}\u{1}\u{2000}\u{1}\u{2600}",
  "\u{1}\u{2000}\u{1}\u{2700}\u{1}\u{2800}\u{1}\u{2900}\u{1}\u{2a00}\u{1}\u{2b00}\u{1}\u{2c00}\u{1}\u{2d00}\u{1}\u{2e00}\u{1}\u{2f00}\u{19}\u{3000}\u{1}\u{3100}\u{51}\u{3000}\u{1}\u{3200}",
  "\u{4}\u{100}\u{1}\u{3300}\u{1}\u{100}\u{1}\u{3400}\u{1}\u{3500}\u{1}\u{3600}\u{1}\u{3700}\u{1}\u{3800}\u{1}\u{3900}\u{2b}\u{1100}\u{1}\u{3a00}\u{21}\u{2000}\u{1}\u{3000}\u{1}\u{3b00}",
  "\u{1}\u{3c00}\u{1}\u{100}\u{1}\u{3d00}\u{1}\u{3e00}\u{1}\u{3f00}\u{1}\u{4000}\u{1}\u{4100}\u{1}\u{4200}\u{1}\u{4300}\u{1}\u{4400}\u{1}\u{4500}\u{1}\u{100}\u{1}\u{4600}\u{1}\u{4700}",
  "\u{1}\u{4800}\u{1}\u{4900}\u{1}\u{4a00}\u{1}\u{4b00}\u{1}\u{4c00}\u{1}\u{2000}\u{1}\u{4d00}\u{1}\u{4e00}\u{1}\u{4f00}\u{1}\u{5000}\u{1}\u{5100}\u{1}\u{5200}\u{1}\u{5300}\u{1}\u{5400}",
  "\u{1}\u{5500}\u{1}\u{5600}\u{1}\u{5700}\u{1}\u{5800}\u{1}\u{2000}\u{1}\u{5900}\u{1}\u{5a00}\u{1}\u{5b00}\u{1}\u{2000}\u{3}\u{100}\u{1}\u{5c00}\u{1}\u{5d00}\u{1}\u{5e00}\u{a}\u{2000}",
  "\u{4}\u{100}\u{1}\u{5f00}\u{f}\u{2000}\u{2}\u{100}\u{1}\u{6000}\u{21}\u{2000}\u{2}\u{100}\u{1}\u{6100}\u{1}\u{6200}\u{2}\u{2000}\u{1}\u{6300}\u{1}\u{6400}\u{40}\u{2000}\u{1}\u{6500}",
  "\u{1}\u{6600}\u{a}\u{2000}\u{1}\u{6700}\u{14}\u{2000}\u{1}\u{6800}\u{1}\u{6900}\u{1}\u{2000}\u{1}\u{6a00}\u{1}\u{6b00}\u{1}\u{6c00}\u{1}\u{6d00}\u{2}\u{2000}\u{1}\u{6e00}\u{5}\u{2000}",
  "\u{1}\u{6f00}\u{1}\u{7000}\u{1}\u{7100}\u{5}\u{2000}\u{1}\u{7200}\u{1}\u{7300}\u{4}\u{2000}\u{1}\u{7400}\u{1}\u{2000}\u{1}\u{7500}\u{1}\u{7600}\u{1}\u{7700}\u{1}\u{7800}\u{1}\u{7900}",
  "\u{1}\u{7a00}\u{1}\u{7b00}\u{1}\u{7c00}\u{1}\u{7d00}\u{1}\u{7e00}\u{1}\u{7f00}\u{4}\u{8000}\u{1}\u{8100}\u{a6}\u{3000}\u{1}\u{8200}\u{10}\u{3000}\u{1}\u{8300}\u{1}\u{8400}\u{15}\u{3000}",
  "\u{1}\u{8500}\u{1c}\u{3000}\u{1}\u{8600}\u{c}\u{2000}\u{2}\u{3000}\u{1}\u{8700}\u{b05}\u{2000}\u{1}\u{8800}\u{1}\u{8900}\u{2fe}\u{2000}",
);
fn zz_unpack_cmap_top() -> Vec<i32> {
  let mut result = vec![0; 4352];
  zz_unpackcmap_blocks_top_with_offset(ZZ_CMAP_TOP_PACKED_0, 0, &mut result);
  result
}
fn zz_unpackcmap_blocks_top_with_offset(packed: &str, offset: usize, result: &mut [i32]) -> usize {
  let mut packed = packed.chars();
  let mut j = offset;
  while let Some(count) = packed.next() {
    let mut count = count as usize;
    let value = expect_invariant!(
      packed.next(),
      "packed tokenizer table has count/value pairs",
    ) as i32;
    while count > 0 {
      result[j] = value;
      j += 1;
      count -= 1;
    }
  }
  j
}
static ZZ_CMAP_BLOCKS: LazyLock<Vec<u8>> = LazyLock::new(zz_unpackcmap_blocks);
const ZZ_CMAP_BLOCKS_PACKED_0: &str = concat!(
  "\u{22}\0\u{1}\u{1}\u{1}\u{2}\u{3}\0\u{1}\u{3}\u{2}\0\u{1}\u{2}\u{1}\0\u{1}\u{4}\u{1}\0\u{1}\u{5}\u{1}\0\u{a}\u{6}\u{1}\u{7}",
  "\u{1}\u{4}\u{5}\0\u{1a}\u{8}\u{4}\0\u{1}\u{9}\u{1}\0\u{1a}\u{8}\u{2e}\0\u{1}\u{a}\u{1}\u{8}\u{2}\0\u{1}\u{b}\u{1}\u{a}\u{6}\0",
  "\u{1}\u{8}\u{1}\0\u{1}\u{7}\u{2}\0\u{1}\u{8}\u{5}\0\u{17}\u{8}\u{1}\0\u{1f}\u{8}\u{1}\0\u{1e0}\u{8}\u{6}\0\u{7}\u{8}\u{7}\0",
  "\u{14}\u{8}\u{70}\u{b}\u{5}\u{8}\u{1}\0\u{2}\u{8}\u{2}\0\u{4}\u{8}\u{1}\u{4}\u{1}\u{8}\u{6}\0\u{1}\u{8}\u{1}\u{7}\u{3}\u{8}\u{1}\0",
  "\u{1}\u{8}\u{1}\0\u{14}\u{8}\u{1}\0\u{53}\u{8}\u{1}\0\u{8b}\u{8}\u{1}\0\u{7}\u{b}\u{a6}\u{8}\u{1}\0\u{26}\u{8}\u{2}\0\u{1}\u{8}",
  "\u{1}\0\u{2}\u{8}\u{1}\0\u{1}\u{8}\u{1}\0\u{29}\u{8}\u{1}\u{4}\u{7}\0\u{2d}\u{b}\u{1}\0\u{1}\u{b}\u{1}\0\u{2}\u{b}\u{1}\0",
  "\u{2}\u{b}\u{1}\0\u{1}\u{b}\u{8}\0\u{1b}\u{c}\u{4}\0\u{4}\u{c}\u{1}\u{8}\u{1}\u{7}\u{b}\0\u{6}\u{b}\u{6}\0\u{2}\u{4}\u{2}\0",
  "\u{b}\u{b}\u{1}\0\u{1}\u{b}\u{3}\0\u{2b}\u{8}\u{15}\u{b}\u{a}\u{d}\u{1}\0\u{1}\u{d}\u{1}\u{4}\u{1}\0\u{2}\u{8}\u{1}\u{b}\u{63}\u{8}",
  "\u{1}\0\u{1}\u{8}\u{8}\u{b}\u{1}\0\u{6}\u{b}\u{2}\u{8}\u{2}\u{b}\u{1}\0\u{4}\u{b}\u{2}\u{8}\u{a}\u{d}\u{3}\u{8}\u{2}\0\u{1}\u{8}",
  "\u{f}\0\u{1}\u{b}\u{1}\u{8}\u{1}\u{b}\u{1e}\u{8}\u{1b}\u{b}\u{2}\0\u{59}\u{8}\u{b}\u{b}\u{1}\u{8}\u{e}\0\u{a}\u{d}\u{21}\u{8}\u{9}\u{b}",
  "\u{2}\u{8}\u{2}\0\u{1}\u{4}\u{1}\0\u{1}\u{8}\u{2}\0\u{1}\u{b}\u{2}\0\u{16}\u{8}\u{4}\u{b}\u{1}\u{8}\u{9}\u{b}\u{1}\u{8}\u{3}\u{b}",
  "\u{1}\u{8}\u{5}\u{b}\u{12}\0\u{19}\u{8}\u{3}\u{b}\u{4}\0\u{b}\u{8}\u{35}\0\u{15}\u{8}\u{1}\0\u{8}\u{8}\u{15}\0\u{31}\u{b}\u{36}\u{8}",
  "\u{3}\u{b}\u{1}\u{8}\u{12}\u{b}\u{1}\u{8}\u{7}\u{b}\u{a}\u{8}\u{2}\u{b}\u{2}\0\u{a}\u{d}\u{1}\0\u{10}\u{8}\u{3}\u{b}\u{1}\0\u{8}\u{8}",
  "\u{2}\0\u{2}\u{8}\u{2}\0\u{16}\u{8}\u{1}\0\u{7}\u{8}\u{1}\0\u{1}\u{8}\u{3}\0\u{4}\u{8}\u{2}\0\u{1}\u{b}\u{1}\u{8}\u{7}\u{b}",
  "\u{2}\0\u{2}\u{b}\u{2}\0\u{3}\u{b}\u{1}\u{8}\u{8}\0\u{1}\u{b}\u{4}\0\u{2}\u{8}\u{1}\0\u{3}\u{8}\u{2}\u{b}\u{2}\0\u{a}\u{d}",
  "\u{2}\u{8}\u{a}\0\u{1}\u{8}\u{1}\0\u{1}\u{b}\u{2}\0\u{3}\u{b}\u{1}\0\u{6}\u{8}\u{4}\0\u{2}\u{8}\u{2}\0\u{16}\u{8}\u{1}\0",
  "\u{7}\u{8}\u{1}\0\u{2}\u{8}\u{1}\0\u{2}\u{8}\u{1}\0\u{2}\u{8}\u{2}\0\u{1}\u{b}\u{1}\0\u{5}\u{b}\u{4}\0\u{2}\u{b}\u{2}\0",
  "\u{3}\u{b}\u{3}\0\u{1}\u{b}\u{7}\0\u{4}\u{8}\u{1}\0\u{1}\u{8}\u{7}\0\u{a}\u{d}\u{2}\u{b}\u{3}\u{8}\u{1}\u{b}\u{b}\0\u{3}\u{b}",
  "\u{1}\0\u{9}\u{8}\u{1}\0\u{3}\u{8}\u{1}\0\u{16}\u{8}\u{1}\0\u{7}\u{8}\u{1}\0\u{2}\u{8}\u{1}\0\u{5}\u{8}\u{2}\0\u{1}\u{b}",
  "\u{1}\u{8}\u{8}\u{b}\u{1}\0\u{3}\u{b}\u{1}\0\u{3}\u{b}\u{2}\0\u{1}\u{8}\u{f}\0\u{2}\u{8}\u{2}\u{b}\u{2}\0\u{a}\u{d}\u{9}\0",
  "\u{1}\u{8}\u{6}\u{b}\u{1}\0\u{3}\u{b}\u{1}\0\u{8}\u{8}\u{2}\0\u{2}\u{8}\u{2}\0\u{16}\u{8}\u{1}\0\u{7}\u{8}\u{1}\0\u{2}\u{8}",
  "\u{1}\0\u{5}\u{8}\u{2}\0\u{1}\u{b}\u{1}\u{8}\u{7}\u{b}\u{2}\0\u{2}\u{b}\u{2}\0\u{3}\u{b}\u{8}\0\u{2}\u{b}\u{4}\0\u{2}\u{8}",
  "\u{1}\0\u{3}\u{8}\u{2}\u{b}\u{2}\0\u{a}\u{d}\u{1}\0\u{1}\u{8}\u{10}\0\u{1}\u{b}\u{1}\u{8}\u{1}\0\u{6}\u{8}\u{3}\0\u{3}\u{8}",
  "\u{1}\0\u{4}\u{8}\u{3}\0\u{2}\u{8}\u{1}\0\u{1}\u{8}\u{1}\0\u{2}\u{8}\u{3}\0\u{2}\u{8}\u{3}\0\u{3}\u{8}\u{3}\0\u{c}\u{8}",
  "\u{4}\0\u{5}\u{b}\u{3}\0\u{3}\u{b}\u{1}\0\u{4}\u{b}\u{2}\0\u{1}\u{8}\u{6}\0\u{1}\u{b}\u{e}\0\u{a}\u{d}\u{10}\0\u{5}\u{b}",
  "\u{8}\u{8}\u{1}\0\u{3}\u{8}\u{1}\0\u{17}\u{8}\u{1}\0\u{10}\u{8}\u{3}\0\u{1}\u{8}\u{7}\u{b}\u{1}\0\u{3}\u{b}\u{1}\0\u{4}\u{b}",
  "\u{7}\0\u{2}\u{b}\u{1}\0\u{3}\u{8}\u{5}\0\u{2}\u{8}\u{2}\u{b}\u{2}\0\u{a}\u{d}\u{10}\0\u{1}\u{8}\u{3}\u{b}\u{1}\0\u{8}\u{8}",
  "\u{1}\0\u{3}\u{8}\u{1}\0\u{17}\u{8}\u{1}\0\u{a}\u{8}\u{1}\0\u{5}\u{8}\u{2}\0\u{1}\u{b}\u{1}\u{8}\u{7}\u{b}\u{1}\0\u{3}\u{b}",
  "\u{1}\0\u{4}\u{b}\u{7}\0\u{2}\u{b}\u{7}\0\u{1}\u{8}\u{1}\0\u{2}\u{8}\u{2}\u{b}\u{2}\0\u{a}\u{d}\u{1}\0\u{2}\u{8}\u{d}\0",
  "\u{4}\u{b}\u{1}\0\u{8}\u{8}\u{1}\0\u{3}\u{8}\u{1}\0\u{29}\u{8}\u{2}\u{b}\u{1}\u{8}\u{7}\u{b}\u{1}\0\u{3}\u{b}\u{1}\0\u{4}\u{b}",
  "\u{1}\u{8}\u{5}\0\u{3}\u{8}\u{1}\u{b}\u{7}\0\u{3}\u{8}\u{2}\u{b}\u{2}\0\u{a}\u{d}\u{a}\0\u{6}\u{8}\u{2}\0\u{2}\u{b}\u{1}\0",
  "\u{12}\u{8}\u{3}\0\u{18}\u{8}\u{1}\0\u{9}\u{8}\u{1}\0\u{1}\u{8}\u{2}\0\u{7}\u{8}\u{3}\0\u{1}\u{b}\u{4}\0\u{6}\u{b}\u{1}\0",
  "\u{1}\u{b}\u{1}\0\u{8}\u{b}\u{6}\0\u{a}\u{d}\u{2}\0\u{2}\u{b}\u{d}\0\u{30}\u{e}\u{1}\u{f}\u{2}\u{e}\u{7}\u{f}\u{5}\0\u{7}\u{e}",
  "\u{8}\u{f}\u{1}\0\u{a}\u{d}\u{27}\0\u{2}\u{e}\u{1}\0\u{1}\u{e}\u{1}\0\u{5}\u{e}\u{1}\0\u{18}\u{e}\u{1}\0\u{1}\u{e}\u{1}\0",
  "\u{a}\u{e}\u{1}\u{f}\u{2}\u{e}\u{9}\u{f}\u{1}\u{e}\u{2}\0\u{5}\u{e}\u{1}\0\u{1}\u{e}\u{1}\0\u{6}\u{f}\u{2}\0\u{a}\u{d}\u{2}\0",
  "\u{4}\u{e}\u{20}\0\u{1}\u{8}\u{17}\0\u{2}\u{b}\u{6}\0\u{a}\u{d}\u{b}\0\u{1}\u{b}\u{1}\0\u{1}\u{b}\u{1}\0\u{1}\u{b}\u{4}\0",
  "\u{2}\u{b}\u{8}\u{8}\u{1}\0\u{24}\u{8}\u{4}\0\u{14}\u{b}\u{1}\0\u{2}\u{b}\u{5}\u{8}\u{b}\u{b}\u{1}\0\u{24}\u{b}\u{9}\0\u{1}\u{b}",
  "\u{39}\0\u{2b}\u{e}\u{14}\u{f}\u{1}\u{e}\u{a}\u{d}\u{6}\0\u{6}\u{e}\u{4}\u{f}\u{4}\u{e}\u{3}\u{f}\u{1}\u{e}\u{3}\u{f}\u{2}\u{e}\u{7}\u{f}",
  "\u{3}\u{e}\u{4}\u{f}\u{d}\u{e}\u{c}\u{f}\u{1}\u{e}\u{1}\u{f}\u{a}\u{d}\u{4}\u{f}\u{2}\u{e}\u{26}\u{8}\u{1}\0\u{1}\u{8}\u{5}\0\u{1}\u{8}",
  "\u{2}\0\u{2b}\u{8}\u{1}\0\u{4}\u{8}\u{100}\u{10}\u{49}\u{8}\u{1}\0\u{4}\u{8}\u{2}\0\u{7}\u{8}\u{1}\0\u{1}\u{8}\u{1}\0\u{4}\u{8}",
  "\u{2}\0\u{29}\u{8}\u{1}\0\u{4}\u{8}\u{2}\0\u{21}\u{8}\u{1}\0\u{4}\u{8}\u{2}\0\u{7}\u{8}\u{1}\0\u{1}\u{8}\u{1}\0\u{4}\u{8}",
  "\u{2}\0\u{f}\u{8}\u{1}\0\u{39}\u{8}\u{1}\0\u{4}\u{8}\u{2}\0\u{43}\u{8}\u{2}\0\u{3}\u{b}\u{20}\0\u{10}\u{8}\u{10}\0\u{56}\u{8}",
  "\u{2}\0\u{6}\u{8}\u{3}\0\u{16c}\u{8}\u{2}\0\u{11}\u{8}\u{1}\0\u{1a}\u{8}\u{5}\0\u{4b}\u{8}\u{3}\0\u{b}\u{8}\u{7}\0\u{d}\u{8}",
  "\u{1}\0\u{4}\u{8}\u{3}\u{b}\u{b}\0\u{12}\u{8}\u{3}\u{b}\u{b}\0\u{12}\u{8}\u{2}\u{b}\u{c}\0\u{d}\u{8}\u{1}\0\u{3}\u{8}\u{1}\0",
  "\u{2}\u{b}\u{c}\0\u{34}\u{e}\u{20}\u{f}\u{3}\0\u{1}\u{e}\u{4}\0\u{1}\u{e}\u{1}\u{f}\u{2}\0\u{a}\u{d}\u{21}\0\u{4}\u{b}\u{1}\0",
  "\u{a}\u{d}\u{6}\0\u{59}\u{8}\u{7}\0\u{5}\u{8}\u{2}\u{b}\u{22}\u{8}\u{1}\u{b}\u{1}\u{8}\u{5}\0\u{46}\u{8}\u{a}\0\u{1f}\u{8}\u{1}\0",
  "\u{c}\u{b}\u{4}\0\u{c}\u{b}\u{a}\0\u{a}\u{d}\u{1e}\u{e}\u{2}\0\u{5}\u{e}\u{b}\0\u{2c}\u{e}\u{4}\0\u{1a}\u{e}\u{6}\0\u{a}\u{d}",
  "\u{1}\u{e}\u{3}\0\u{2}\u{e}\u{20}\0\u{17}\u{8}\u{5}\u{b}\u{4}\0\u{35}\u{e}\u{a}\u{f}\u{1}\0\u{1d}\u{f}\u{2}\0\u{1}\u{b}\u{a}\u{d}",
  "\u{6}\0\u{a}\u{d}\u{6}\0\u{e}\u{e}\u{2}\0\u{f}\u{b}\u{41}\0\u{5}\u{b}\u{2f}\u{8}\u{11}\u{b}\u{7}\u{8}\u{4}\0\u{a}\u{d}\u{11}\0",
  "\u{9}\u{b}\u{c}\0\u{3}\u{b}\u{1e}\u{8}\u{d}\u{b}\u{2}\u{8}\u{a}\u{d}\u{2c}\u{8}\u{e}\u{b}\u{c}\0\u{24}\u{8}\u{14}\u{b}\u{8}\0\u{a}\u{d}",
  "\u{3}\0\u{3}\u{8}\u{a}\u{d}\u{24}\u{8}\u{2}\0\u{9}\u{8}\u{7}\0\u{2b}\u{8}\u{2}\0\u{3}\u{8}\u{10}\0\u{3}\u{b}\u{1}\0\u{15}\u{b}",
  "\u{4}\u{8}\u{1}\u{b}\u{6}\u{8}\u{1}\u{b}\u{2}\u{8}\u{3}\u{b}\u{1}\u{8}\u{5}\0\u{c0}\u{8}\u{3a}\u{b}\u{1}\0\u{5}\u{b}\u{16}\u{8}\u{2}\0",
  "\u{6}\u{8}\u{2}\0\u{26}\u{8}\u{2}\0\u{6}\u{8}\u{2}\0\u{8}\u{8}\u{1}\0\u{1}\u{8}\u{1}\0\u{1}\u{8}\u{1}\0\u{1}\u{8}\u{1}\0",
  "\u{1f}\u{8}\u{2}\0\u{35}\u{8}\u{1}\0\u{7}\u{8}\u{1}\0\u{1}\u{8}\u{3}\0\u{3}\u{8}\u{1}\0\u{7}\u{8}\u{3}\0\u{4}\u{8}\u{2}\0",
  "\u{6}\u{8}\u{4}\0\u{d}\u{8}\u{5}\0\u{3}\u{8}\u{1}\0\u{7}\u{8}\u{f}\0\u{1}\u{b}\u{1}\u{11}\u{2}\u{b}\u{8}\0\u{2}\u{5}\u{a}\0",
  "\u{1}\u{5}\u{2}\0\u{1}\u{7}\u{2}\0\u{5}\u{b}\u{1}\u{9}\u{c}\0\u{1}\u{a}\u{2}\0\u{2}\u{9}\u{3}\0\u{1}\u{4}\u{4}\0\u{1}\u{a}",
  "\u{a}\0\u{1}\u{9}\u{b}\0\u{5}\u{b}\u{1}\0\u{a}\u{b}\u{1}\0\u{1}\u{8}\u{d}\0\u{1}\u{8}\u{10}\0\u{d}\u{8}\u{33}\0\u{13}\u{b}",
  "\u{1}\u{12}\u{d}\u{b}\u{11}\0\u{1}\u{8}\u{4}\0\u{1}\u{8}\u{2}\0\u{a}\u{8}\u{1}\0\u{1}\u{8}\u{3}\0\u{5}\u{8}\u{4}\0\u{1}\u{a}",
  "\u{1}\0\u{1}\u{8}\u{1}\0\u{1}\u{8}\u{1}\0\u{1}\u{8}\u{1}\0\u{4}\u{8}\u{1}\0\u{a}\u{8}\u{1}\u{13}\u{2}\0\u{4}\u{8}\u{5}\0",
  "\u{5}\u{8}\u{4}\0\u{1}\u{8}\u{11}\0\u{29}\u{8}\u{b}\0\u{6}\u{a}\u{f}\0\u{2}\u{a}\u{16f}\0\u{2}\u{a}\u{c}\0\u{1}\u{a}\u{5f}\0",
  "\u{1}\u{a}\u{46}\0\u{1}\u{a}\u{19}\0\u{b}\u{a}\u{4}\0\u{3}\u{a}\u{bb}\0\u{c}\u{8}\u{1}\u{13}\u{27}\u{8}\u{c0}\0\u{2}\u{a}\u{a}\0",
  "\u{1}\u{a}\u{9}\0\u{1}\u{a}\u{3a}\0\u{4}\u{a}\u{1}\0\u{6}\u{a}\u{1}\0\u{c}\u{a}\u{1}\0\u{72}\u{a}\u{a}\0\u{76}\u{a}\u{2}\0",
  "\u{b}\u{a}\u{1}\0\u{1}\u{a}\u{1}\0\u{1}\u{a}\u{6}\0\u{1}\u{a}\u{3}\0\u{1}\u{a}\u{6}\0\u{1}\u{a}\u{a}\0\u{2}\u{a}\u{f}\0",
  "\u{1}\u{a}\u{2}\0\u{1}\u{a}\u{4}\0\u{1}\u{a}\u{1}\0\u{1}\u{a}\u{4}\0\u{3}\u{a}\u{1}\0\u{1}\u{a}\u{b}\0\u{5}\u{a}\u{2d}\0",
  "\u{3}\u{a}\u{9}\0\u{1}\u{a}\u{e}\0\u{1}\u{a}\u{e}\0\u{1}\u{a}\u{74}\0\u{2}\u{a}\u{cf}\0\u{3}\u{a}\u{13}\0\u{2}\u{a}\u{33}\0",
  "\u{1}\u{a}\u{4}\0\u{1}\u{a}\u{aa}\0\u{2f}\u{8}\u{1}\0\u{2f}\u{8}\u{1}\0\u{85}\u{8}\u{6}\0\u{4}\u{8}\u{3}\u{b}\u{2}\u{8}\u{c}\0",
  "\u{26}\u{8}\u{1}\0\u{1}\u{8}\u{5}\0\u{1}\u{8}\u{2}\0\u{38}\u{8}\u{7}\0\u{1}\u{8}\u{f}\0\u{1}\u{b}\u{17}\u{8}\u{9}\0\u{7}\u{8}",
  "\u{1}\0\u{7}\u{8}\u{1}\0\u{7}\u{8}\u{1}\0\u{7}\u{8}\u{1}\0\u{7}\u{8}\u{1}\0\u{7}\u{8}\u{1}\0\u{7}\u{8}\u{1}\0\u{7}\u{8}",
  "\u{1}\0\u{20}\u{b}\u{2f}\0\u{1}\u{8}\u{50}\0\u{1a}\u{14}\u{1}\0\u{59}\u{14}\u{c}\0\u{d6}\u{14}\u{2f}\0\u{1}\u{8}\u{1}\0\u{1}\u{14}",
  "\u{19}\0\u{9}\u{14}\u{6}\u{b}\u{1}\u{a}\u{5}\u{15}\u{2}\0\u{3}\u{14}\u{2}\u{8}\u{1}\u{a}\u{3}\0\u{56}\u{16}\u{2}\0\u{2}\u{b}\u{2}\u{15}",
  "\u{3}\u{16}\u{5b}\u{15}\u{1}\0\u{4}\u{15}\u{5}\0\u{2b}\u{8}\u{1}\0\u{5e}\u{10}\u{11}\0\u{1b}\u{8}\u{35}\0\u{10}\u{15}\u{97}\0\u{1}\u{a}",
  "\u{1}\0\u{1}\u{a}\u{36}\0\u{2f}\u{15}\u{1}\0\u{58}\u{15}\u{a8}\0\u{1b6}\u{14}\u{4a}\0\u{f0}\u{14}\u{10}\0\u{8d}\u{8}\u{43}\0\u{2e}\u{8}",
  "\u{2}\0\u{d}\u{8}\u{3}\0\u{10}\u{8}\u{a}\u{d}\u{2}\u{8}\u{14}\0\u{2f}\u{8}\u{4}\u{b}\u{1}\0\u{a}\u{b}\u{1}\0\u{1f}\u{8}\u{2}\u{b}",
  "\u{50}\u{8}\u{2}\u{b}\u{25}\0\u{a9}\u{8}\u{2}\0\u{5}\u{8}\u{30}\0\u{b}\u{8}\u{1}\u{b}\u{3}\u{8}\u{1}\u{b}\u{4}\u{8}\u{1}\u{b}\u{17}\u{8}",
  "\u{5}\u{b}\u{18}\0\u{34}\u{8}\u{c}\0\u{2}\u{b}\u{32}\u{8}\u{12}\u{b}\u{a}\0\u{a}\u{d}\u{6}\0\u{12}\u{b}\u{6}\u{8}\u{3}\0\u{1}\u{8}",
  "\u{1}\0\u{2}\u{8}\u{1}\u{b}\u{a}\u{d}\u{1c}\u{8}\u{8}\u{b}\u{2}\0\u{17}\u{8}\u{d}\u{b}\u{c}\0\u{1d}\u{10}\u{3}\0\u{4}\u{b}\u{2f}\u{8}",
  "\u{e}\u{b}\u{e}\0\u{1}\u{8}\u{a}\u{d}\u{6}\0\u{5}\u{e}\u{1}\u{f}\u{a}\u{e}\u{a}\u{d}\u{5}\u{e}\u{1}\0\u{29}\u{8}\u{e}\u{b}\u{9}\0",
  "\u{3}\u{8}\u{1}\u{b}\u{8}\u{8}\u{2}\u{b}\u{2}\0\u{a}\u{d}\u{6}\0\u{1b}\u{e}\u{3}\u{f}\u{32}\u{e}\u{1}\u{f}\u{1}\u{e}\u{3}\u{f}\u{2}\u{e}",
  "\u{2}\u{f}\u{5}\u{e}\u{2}\u{f}\u{1}\u{e}\u{1}\u{f}\u{1}\u{e}\u{18}\0\u{5}\u{e}\u{b}\u{8}\u{5}\u{b}\u{2}\0\u{3}\u{8}\u{2}\u{b}\u{a}\0",
  "\u{6}\u{8}\u{2}\0\u{6}\u{8}\u{2}\0\u{6}\u{8}\u{9}\0\u{7}\u{8}\u{1}\0\u{7}\u{8}\u{1}\0\u{38}\u{8}\u{8}\0\u{73}\u{8}\u{8}\u{b}",
  "\u{1}\0\u{2}\u{b}\u{2}\0\u{a}\u{d}\u{6}\0\u{a4}\u{10}\u{c}\0\u{17}\u{10}\u{4}\0\u{31}\u{10}\u{4}\0\u{6e}\u{14}\u{2}\0\u{6a}\u{14}",
  "\u{26}\0\u{7}\u{8}\u{c}\0\u{5}\u{8}\u{5}\0\u{1}\u{c}\u{1}\u{b}\u{a}\u{c}\u{1}\0\u{d}\u{c}\u{1}\0\u{5}\u{c}\u{1}\0\u{1}\u{c}",
  "\u{1}\0\u{2}\u{c}\u{1}\0\u{2}\u{c}\u{1}\0\u{a}\u{c}\u{62}\u{8}\u{21}\0\u{6b}\u{8}\u{12}\0\u{40}\u{8}\u{2}\0\u{36}\u{8}\u{28}\0",
  "\u{c}\u{8}\u{4}\0\u{e}\u{b}\u{1}\u{17}\u{1}\u{18}\u{1}\u{4}\u{2}\0\u{1}\u{7}\u{1}\u{4}\u{b}\0\u{10}\u{b}\u{3}\0\u{2}\u{9}\u{18}\0",
  "\u{3}\u{9}\u{1}\u{4}\u{1}\0\u{1}\u{5}\u{1}\0\u{1}\u{4}\u{1}\u{7}\u{1a}\0\u{5}\u{8}\u{1}\0\u{87}\u{8}\u{2}\0\u{1}\u{b}\u{7}\0",
  "\u{1}\u{5}\u{4}\0\u{1}\u{4}\u{1}\0\u{1}\u{5}\u{1}\0\u{a}\u{d}\u{1}\u{7}\u{1}\u{4}\u{5}\0\u{1a}\u{8}\u{4}\0\u{1}\u{9}\u{1}\0",
  "\u{1a}\u{8}\u{b}\0\u{38}\u{15}\u{2}\u{b}\u{1f}\u{10}\u{3}\0\u{6}\u{10}\u{2}\0\u{6}\u{10}\u{2}\0\u{6}\u{10}\u{2}\0\u{3}\u{10}\u{1c}\0",
  "\u{3}\u{b}\u{4}\0\u{c}\u{8}\u{1}\0\u{1a}\u{8}\u{1}\0\u{13}\u{8}\u{1}\0\u{2}\u{8}\u{1}\0\u{f}\u{8}\u{2}\0\u{e}\u{8}\u{22}\0",
  "\u{7b}\u{8}\u{45}\0\u{35}\u{8}\u{88}\0\u{1}\u{b}\u{82}\0\u{1d}\u{8}\u{3}\0\u{31}\u{8}\u{f}\0\u{1}\u{b}\u{1f}\0\u{20}\u{8}\u{d}\0",
  "\u{1e}\u{8}\u{5}\0\u{26}\u{8}\u{5}\u{b}\u{5}\0\u{1e}\u{8}\u{2}\0\u{24}\u{8}\u{4}\0\u{8}\u{8}\u{1}\0\u{5}\u{8}\u{2a}\0\u{9e}\u{8}",
  "\u{2}\0\u{a}\u{d}\u{6}\0\u{24}\u{8}\u{4}\0\u{24}\u{8}\u{4}\0\u{28}\u{8}\u{8}\0\u{34}\u{8}\u{9c}\0\u{37}\u{8}\u{9}\0\u{16}\u{8}",
  "\u{a}\0\u{8}\u{8}\u{98}\0\u{6}\u{8}\u{2}\0\u{1}\u{8}\u{1}\0\u{2c}\u{8}\u{1}\0\u{2}\u{8}\u{3}\0\u{1}\u{8}\u{2}\0\u{17}\u{8}",
  "\u{a}\0\u{17}\u{8}\u{9}\0\u{1f}\u{8}\u{41}\0\u{13}\u{8}\u{1}\0\u{2}\u{8}\u{a}\0\u{16}\u{8}\u{a}\0\u{1a}\u{8}\u{46}\0\u{38}\u{8}",
  "\u{6}\0\u{2}\u{8}\u{40}\0\u{1}\u{8}\u{3}\u{b}\u{1}\0\u{2}\u{b}\u{5}\0\u{4}\u{b}\u{4}\u{8}\u{1}\0\u{3}\u{8}\u{1}\0\u{1d}\u{8}",
  "\u{2}\0\u{3}\u{b}\u{4}\0\u{1}\u{b}\u{20}\0\u{1d}\u{8}\u{3}\0\u{1d}\u{8}\u{23}\0\u{8}\u{8}\u{1}\0\u{1c}\u{8}\u{2}\u{b}\u{19}\0",
  "\u{36}\u{8}\u{a}\0\u{16}\u{8}\u{a}\0\u{13}\u{8}\u{d}\0\u{12}\u{8}\u{6e}\0\u{49}\u{8}\u{37}\0\u{33}\u{8}\u{d}\0\u{33}\u{8}\u{d}\0",
  "\u{24}\u{8}\u{4}\u{b}\u{8}\0\u{a}\u{d}\u{c6}\0\u{1d}\u{8}\u{a}\0\u{1}\u{8}\u{8}\0\u{16}\u{8}\u{b}\u{b}\u{8f}\0\u{17}\u{8}\u{9}\0",
  "\u{3}\u{b}\u{35}\u{8}\u{f}\u{b}\u{1f}\0\u{a}\u{d}\u{f}\0\u{4}\u{b}\u{2d}\u{8}\u{b}\u{b}\u{2}\0\u{1}\u{b}\u{f}\0\u{1}\u{b}\u{2}\0",
  "\u{19}\u{8}\u{7}\0\u{a}\u{d}\u{6}\0\u{3}\u{b}\u{24}\u{8}\u{e}\u{b}\u{1}\0\u{a}\u{d}\u{4}\0\u{1}\u{8}\u{2}\u{b}\u{9}\0\u{23}\u{8}",
  "\u{1}\u{b}\u{2}\0\u{1}\u{8}\u{9}\0\u{3}\u{b}\u{30}\u{8}\u{e}\u{b}\u{4}\u{8}\u{4}\0\u{4}\u{b}\u{3}\0\u{a}\u{d}\u{1}\u{8}\u{1}\0",
  "\u{1}\u{8}\u{23}\0\u{12}\u{8}\u{1}\0\u{19}\u{8}\u{c}\u{b}\u{6}\0\u{1}\u{b}\u{41}\0\u{7}\u{8}\u{1}\0\u{1}\u{8}\u{1}\0\u{4}\u{8}",
  "\u{1}\0\u{f}\u{8}\u{1}\0\u{a}\u{8}\u{7}\0\u{2f}\u{8}\u{c}\u{b}\u{5}\0\u{a}\u{d}\u{6}\0\u{4}\u{b}\u{1}\0\u{8}\u{8}\u{2}\0",
  "\u{2}\u{8}\u{2}\0\u{16}\u{8}\u{1}\0\u{7}\u{8}\u{1}\0\u{2}\u{8}\u{1}\0\u{5}\u{8}\u{1}\0\u{2}\u{b}\u{1}\u{8}\u{7}\u{b}\u{2}\0",
  "\u{2}\u{b}\u{2}\0\u{3}\u{b}\u{2}\0\u{1}\u{8}\u{6}\0\u{1}\u{b}\u{5}\0\u{5}\u{8}\u{2}\u{b}\u{2}\0\u{7}\u{b}\u{3}\0\u{5}\u{b}",
  "\u{8b}\0\u{35}\u{8}\u{12}\u{b}\u{4}\u{8}\u{5}\0\u{a}\u{d}\u{4}\0\u{1}\u{b}\u{1}\u{8}\u{20}\0\u{30}\u{8}\u{14}\u{b}\u{2}\u{8}\u{1}\0",
  "\u{1}\u{8}\u{8}\0\u{a}\u{d}\u{a6}\0\u{2f}\u{8}\u{7}\u{b}\u{2}\0\u{9}\u{b}\u{17}\0\u{4}\u{8}\u{2}\u{b}\u{22}\0\u{30}\u{8}\u{11}\u{b}",
  "\u{3}\0\u{1}\u{8}\u{b}\0\u{a}\u{d}\u{26}\0\u{2b}\u{8}\u{d}\u{b}\u{1}\u{8}\u{7}\0\u{a}\u{d}\u{36}\0\u{1b}\u{e}\u{2}\0\u{f}\u{f}",
  "\u{4}\0\u{a}\u{d}\u{2}\u{e}\u{3}\0\u{1}\u{e}\u{c0}\0\u{2c}\u{8}\u{f}\u{b}\u{65}\0\u{40}\u{8}\u{a}\u{d}\u{15}\0\u{1}\u{8}\u{a0}\0",
  "\u{8}\u{8}\u{2}\0\u{27}\u{8}\u{7}\u{b}\u{2}\0\u{7}\u{b}\u{1}\u{8}\u{1}\0\u{1}\u{8}\u{1}\u{b}\u{1b}\0\u{1}\u{8}\u{a}\u{b}\u{28}\u{8}",
  "\u{7}\u{b}\u{1}\u{8}\u{4}\u{b}\u{8}\0\u{1}\u{b}\u{8}\0\u{1}\u{8}\u{b}\u{b}\u{2e}\u{8}\u{10}\u{b}\u{3}\0\u{1}\u{8}\u{22}\0\u{39}\u{8}",
  "\u{7}\0\u{9}\u{8}\u{1}\0\u{25}\u{8}\u{8}\u{b}\u{1}\0\u{8}\u{b}\u{1}\u{8}\u{f}\0\u{a}\u{d}\u{18}\0\u{1e}\u{8}\u{2}\0\u{16}\u{b}",
  "\u{1}\0\u{e}\u{b}\u{49}\0\u{7}\u{8}\u{1}\0\u{2}\u{8}\u{1}\0\u{26}\u{8}\u{6}\u{b}\u{3}\0\u{1}\u{b}\u{1}\0\u{2}\u{b}\u{1}\0",
  "\u{7}\u{b}\u{1}\u{8}\u{1}\u{b}\u{8}\0\u{a}\u{d}\u{6}\0\u{6}\u{8}\u{1}\0\u{2}\u{8}\u{1}\0\u{20}\u{8}\u{5}\u{b}\u{1}\0\u{2}\u{b}",
  "\u{1}\0\u{5}\u{b}\u{1}\u{8}\u{7}\0\u{a}\u{d}\u{136}\0\u{13}\u{8}\u{4}\u{b}\u{9}\0\u{9a}\u{8}\u{66}\0\u{6f}\u{8}\u{11}\0\u{c4}\u{8}",
  "\u{bc}\0\u{2f}\u{8}\u{1}\0\u{9}\u{b}\u{c7}\0\u{47}\u{8}\u{b9}\0\u{39}\u{8}\u{7}\0\u{1f}\u{8}\u{1}\0\u{a}\u{d}\u{66}\0\u{1e}\u{8}",
  "\u{2}\0\u{5}\u{b}\u{b}\0\u{30}\u{8}\u{7}\u{b}\u{9}\0\u{4}\u{8}\u{c}\0\u{a}\u{d}\u{9}\0\u{15}\u{8}\u{5}\0\u{13}\u{8}\u{b0}\0",
  "\u{40}\u{8}\u{80}\0\u{4b}\u{8}\u{4}\0\u{1}\u{b}\u{1}\u{8}\u{37}\u{b}\u{7}\0\u{4}\u{b}\u{d}\u{8}\u{40}\0\u{2}\u{8}\u{1}\0\u{1}\u{8}",
  "\u{1c}\0\u{1}\u{15}\u{11e}\u{16}\u{31}\0\u{3}\u{16}\u{11}\0\u{4}\u{15}\u{98}\0\u{6b}\u{8}\u{5}\0\u{d}\u{8}\u{3}\0\u{9}\u{8}\u{7}\0",
  "\u{a}\u{8}\u{3}\0\u{2}\u{b}\u{1}\0\u{4}\u{b}\u{c1}\0\u{5}\u{b}\u{3}\0\u{16}\u{b}\u{2}\0\u{7}\u{b}\u{1e}\0\u{4}\u{b}\u{94}\0",
  "\u{3}\u{b}\u{bb}\0\u{55}\u{8}\u{1}\0\u{47}\u{8}\u{1}\0\u{2}\u{8}\u{2}\0\u{1}\u{8}\u{2}\0\u{2}\u{8}\u{2}\0\u{4}\u{8}\u{1}\0",
  "\u{c}\u{8}\u{1}\0\u{1}\u{8}\u{1}\0\u{7}\u{8}\u{1}\0\u{41}\u{8}\u{1}\0\u{4}\u{8}\u{2}\0\u{8}\u{8}\u{1}\0\u{7}\u{8}\u{1}\0",
  "\u{1c}\u{8}\u{1}\0\u{4}\u{8}\u{1}\0\u{5}\u{8}\u{1}\0\u{1}\u{8}\u{3}\0\u{7}\u{8}\u{1}\0\u{154}\u{8}\u{2}\0\u{19}\u{8}\u{1}\0",
  "\u{19}\u{8}\u{1}\0\u{1f}\u{8}\u{1}\0\u{19}\u{8}\u{1}\0\u{1f}\u{8}\u{1}\0\u{19}\u{8}\u{1}\0\u{1f}\u{8}\u{1}\0\u{19}\u{8}\u{1}\0",
  "\u{1f}\u{8}\u{1}\0\u{19}\u{8}\u{1}\0\u{8}\u{8}\u{2}\0\u{32}\u{d}\u{37}\u{b}\u{4}\0\u{32}\u{b}\u{8}\0\u{1}\u{b}\u{e}\0\u{1}\u{b}",
  "\u{16}\0\u{5}\u{b}\u{1}\0\u{f}\u{b}\u{50}\0\u{7}\u{b}\u{1}\0\u{11}\u{b}\u{2}\0\u{7}\u{b}\u{1}\0\u{2}\u{b}\u{1}\0\u{5}\u{b}",
  "\u{d5}\0\u{2d}\u{8}\u{3}\0\u{7}\u{b}\u{7}\u{8}\u{2}\0\u{a}\u{d}\u{4}\0\u{1}\u{8}\u{171}\0\u{2c}\u{8}\u{4}\u{b}\u{a}\u{d}\u{6}\0",
  "\u{c5}\u{8}\u{b}\0\u{7}\u{b}\u{29}\0\u{44}\u{8}\u{7}\u{b}\u{1}\u{8}\u{4}\0\u{a}\u{d}\u{a6}\0\u{4}\u{8}\u{1}\0\u{1b}\u{8}\u{1}\0",
  "\u{2}\u{8}\u{1}\0\u{1}\u{8}\u{2}\0\u{1}\u{8}\u{1}\0\u{a}\u{8}\u{1}\0\u{4}\u{8}\u{1}\0\u{1}\u{8}\u{1}\0\u{1}\u{8}\u{6}\0",
  "\u{1}\u{8}\u{4}\0\u{1}\u{8}\u{1}\0\u{1}\u{8}\u{1}\0\u{1}\u{8}\u{1}\0\u{3}\u{8}\u{1}\0\u{2}\u{8}\u{1}\0\u{1}\u{8}\u{2}\0",
  "\u{1}\u{8}\u{1}\0\u{1}\u{8}\u{1}\0\u{1}\u{8}\u{1}\0\u{1}\u{8}\u{1}\0\u{1}\u{8}\u{1}\0\u{2}\u{8}\u{1}\0\u{1}\u{8}\u{2}\0",
  "\u{4}\u{8}\u{1}\0\u{7}\u{8}\u{1}\0\u{4}\u{8}\u{1}\0\u{4}\u{8}\u{1}\0\u{1}\u{8}\u{1}\0\u{a}\u{8}\u{1}\0\u{11}\u{8}\u{5}\0",
  "\u{3}\u{8}\u{1}\0\u{5}\u{8}\u{1}\0\u{11}\u{8}\u{44}\0\u{100}\u{a}\u{d}\0\u{3}\u{a}\u{1f}\0\u{1}\u{a}\u{1a}\u{8}\u{6}\0\u{1a}\u{8}",
  "\u{2}\0\u{4}\u{a}\u{2}\u{13}\u{c}\u{8}\u{2}\u{13}\u{a}\u{8}\u{4}\0\u{1}\u{a}\u{2}\0\u{a}\u{a}\u{12}\0\u{39}\u{a}\u{1a}\u{19}\u{1}\u{16}",
  "\u{f}\u{a}\u{a}\0\u{1}\u{a}\u{14}\0\u{1}\u{a}\u{2}\0\u{9}\u{a}\u{1}\0\u{4}\u{a}\u{9}\0\u{1b2}\u{a}\u{5}\u{1a}\u{13e}\u{a}\u{8}\0",
  "\u{10a}\u{a}\u{30}\0\u{80}\u{a}\u{74}\0\u{c}\u{a}\u{55}\0\u{2b}\u{a}\u{c}\0\u{4}\u{a}\u{38}\0\u{8}\u{a}\u{a}\0\u{6}\u{a}\u{28}\0",
  "\u{8}\u{a}\u{1e}\0\u{52}\u{a}\u{c}\0\u{2f}\u{a}\u{1}\0\u{a}\u{a}\u{1}\0\u{3b7}\u{a}\u{2}\0\u{d7}\u{14}\u{29}\0\u{35}\u{14}\u{b}\0",
  "\u{de}\u{14}\u{2}\0\u{182}\u{14}\u{e}\0\u{131}\u{14}\u{1f}\0\u{1e}\u{14}\u{e3}\0\u{1}\u{b}\u{1e}\0\u{5f}\u{1b}\u{1}\u{1c}\u{80}\0\u{f0}\u{b}",
  "\u{10}\0",
);
fn zz_unpackcmap_blocks() -> Vec<u8> {
  let mut result = vec![0; 35328];
  zz_unpackcmap_blocks_with_offset(ZZ_CMAP_BLOCKS_PACKED_0, 0, &mut result);
  result
}
fn zz_unpackcmap_blocks_with_offset(packed: &str, offset: usize, result: &mut [u8]) -> usize {
  let mut packed = packed.chars();
  let mut j = offset;
  while let Some(count) = packed.next() {
    let mut count = count as usize;
    let value = expect_invariant!(
      u8::try_from(expect_invariant!(
        packed.next(),
        "packed tokenizer table has count/value pairs",
      ) as u32),
      "packed tokenizer character class fits in u8",
    );
    while count > 0 {
      result[j] = value;
      j += 1;
      count -= 1;
    }
  }
  j
}

static ZZ_ACTION: LazyLock<[i32; 59]> = LazyLock::new(zz_unpack_action);

const ZZ_ACTION_PACKED_0: &str = "\u{1}\0\u{2}\u{1}\u{1}\u{2}\u{1}\u{3}\u{1}\u{1}\u{1}\u{4}\u{1}\u{3}\u{1}\u{2}\
\u{1}\u{5}\u{1}\u{6}\u{1}\u{1}\u{1}\u{4}\u{1}\u{7}\u{1}\u{8}\u{1}\u{9}\u{1}\u{1}\
\u{1}\u{4}\u{1}\0\u{1}\u{4}\u{2}\0\u{1}\u{2}\u{1}\u{4}\u{1}\u{2}\u{1}\0\
\u{2}\u{3}\u{1}\0\u{1}\u{3}\u{2}\u{4}\u{1}\0\u{1}\u{3}\u{1}\0\u{2}\u{4}\
\u{1}\0\u{4}\u{4}\u{1}\0\u{1}\u{4}\u{2}\0\u{1}\u{4}\u{2}\u{3}\u{2}\u{4}\
\u{1}\0\u{3}\u{4}\u{1}\u{3}\u{3}\u{4}";

fn zz_unpack_action() -> [i32; 59] {
  let mut result = [0; 59];
  zz_unpack_action_with_offset(ZZ_ACTION_PACKED_0, 0, &mut result);
  result
}
fn zz_unpack_action_with_offset(packed: &str, offset: usize, result: &mut [i32]) -> usize {
  let mut packed = packed.chars();
  let mut j = offset;
  while let Some(count) = packed.next() {
    let mut count = count as usize;
    let value = expect_invariant!(
      packed.next(),
      "packed tokenizer action has count/value pairs"
    ) as i32;
    while count > 0 {
      result[j] = value;
      j += 1;
      count -= 1;
    }
  }
  j
}

/// Translates a state to a row index in the transition table
static ZZ_ROWMAP: LazyLock<[usize; 59]> = LazyLock::new(zz_unpack_row_map);
const ZZ_ROWMAP_PACKED_0: &str = "\0\0\0\u{1d}\0\u{3a}\0\u{57}\0\u{74}\0\u{91}\0\u{ae}\0\u{cb}\
\0\u{e8}\0\u{105}\0\u{122}\0\u{13f}\0\u{15c}\0\u{179}\0\u{196}\0\u{1b3}\
\0\u{1d0}\0\u{1ed}\0\u{3a}\0\u{20a}\0\u{227}\0\u{244}\0\u{261}\0\u{27e}\
\0\u{29b}\0\u{2b8}\0\u{2d5}\0\u{2f2}\0\u{91}\0\u{30f}\0\u{32c}\0\u{349}\
\0\u{366}\0\u{383}\0\u{13f}\0\u{3a0}\0\u{3bd}\0\u{1d0}\0\u{3da}\0\u{3f7}\
\0\u{414}\0\u{431}\0\u{44e}\0\u{46b}\0\u{488}\0\u{4a5}\0\u{4c2}\0\u{4df}\
\0\u{4fc}\0\u{519}\0\u{536}\0\u{553}\0\u{1d}\0\u{570}\0\u{58d}\0\u{5aa}\
\0\u{5c7}\0\u{74}\0\u{5e4}";
fn zz_unpack_row_map() -> [usize; 59] {
  let mut result = [0; 59];
  zz_unpack_row_map_with_offset(ZZ_ROWMAP_PACKED_0, 0, &mut result);
  result
}
fn zz_unpack_row_map_with_offset(packed: &str, _offset: usize, result: &mut [usize]) -> usize {
  let mut packed = packed.chars();
  let mut j = 0;
  while let Some(high) = packed.next() {
    let high = (high as usize) << 16;
    let low = expect_invariant!(packed.next(), "packed tokenizer row map has high/low pairs");
    result[j] = high | low as usize;
    j += 1;
  }
  j
}
// Cover the u8 class domain so a valid class needs no column bounds check.
const ZZ_TRANS_WIDTH: usize = u8::MAX as usize + 1;
static ZZ_TRANS: LazyLock<Vec<[i32; ZZ_TRANS_WIDTH]>> = LazyLock::new(zz_unpack_trans);
const ZZ_TRANS_PACKED_0: &str = concat!(
  "\u{2}\u{2}\u{1}\u{3}\u{3}\u{2}\u{1}\u{4}\u{1}\u{2}\u{1}\u{5}\u{1}\u{6}\u{1}\u{7}\u{1}\u{2}\u{1}\u{8}\u{1}\u{9}\u{2}\u{a}\u{1}\u{b}\u{1}\u{c}",
  "\u{1}\u{2}\u{1}\u{d}\u{1}\u{e}\u{1}\u{f}\u{1}\u{10}\u{2}\u{2}\u{1}\u{11}\u{1}\u{12}\u{2}\u{2}\u{28}\0\u{1}\u{13}\u{3}\0\u{1}\u{13}\u{1}\0",
  "\u{1}\u{13}\u{1}\u{14}\u{5}\0\u{1}\u{15}\u{1}\0\u{3}\u{13}\u{3}\0\u{3}\u{16}\u{1}\u{9}\u{1}\0\u{1}\u{5}\u{1}\u{17}\u{1}\0\u{1}\u{4}",
  "\u{1}\u{8}\u{1}\u{9}\u{1}\0\u{1}\u{4}\u{1}\u{5}\u{1}\u{4}\u{1}\u{18}\u{1}\u{5}\u{3}\0\u{1}\u{9}\u{1}\u{19}\u{1}\0\u{3}\u{4}\u{3}\0",
  "\u{1}\u{1a}\u{1}\0\u{1}\u{1a}\u{1}\u{1b}\u{1}\u{1a}\u{1}\u{5}\u{1}\u{1c}\u{1}\0\u{1}\u{5}\u{1}\u{8}\u{1}\u{1b}\u{1}\0\u{5}\u{5}\u{3}\0",
  "\u{2}\u{5}\u{1}\0\u{3}\u{5}\u{6}\0\u{1}\u{9}\u{1}\0\u{1}\u{5}\u{1}\u{1d}\u{1}\0\u{1}\u{1d}\u{1}\u{8}\u{1}\u{9}\u{1}\0\u{1}\u{1d}",
  "\u{1}\u{5}\u{2}\u{1d}\u{1}\u{5}\u{1}\0\u{1}\u{1e}\u{1}\0\u{2}\u{1d}\u{1}\0\u{3}\u{1d}\u{b}\0\u{1}\u{7}\u{3}\0\u{1}\u{7}\u{1}\0",
  "\u{1}\u{1f}\u{1}\u{7}\u{5}\0\u{1}\u{20}\u{1}\0\u{3}\u{7}\u{1}\0\u{1}\u{21}\u{1}\0\u{1}\u{22}\u{1}\0\u{1}\u{1a}\u{1}\u{1b}\u{1}\u{1a}",
  "\u{1}\u{5}\u{1}\u{1c}\u{1}\0\u{2}\u{8}\u{1}\u{1b}\u{1}\0\u{1}\u{8}\u{1}\u{5}\u{2}\u{8}\u{1}\u{5}\u{3}\0\u{2}\u{8}\u{1}\0\u{3}\u{8}",
  "\u{3}\0\u{3}\u{16}\u{1}\u{9}\u{1}\0\u{1}\u{5}\u{1}\u{17}\u{1}\0\u{1}\u{9}\u{1}\u{8}\u{1}\u{9}\u{1}\0\u{1}\u{9}\u{1}\u{5}\u{2}\u{9}",
  "\u{1}\u{5}\u{3}\0\u{2}\u{9}\u{1}\0\u{3}\u{9}\u{b}\0\u{1}\u{a}\u{2}\0\u{2}\u{a}\u{1}\0\u{2}\u{a}\u{4}\0\u{2}\u{a}\u{1}\0",
  "\u{3}\u{a}\u{3}\0\u{1}\u{1a}\u{1}\0\u{1}\u{1a}\u{1}\u{1b}\u{1}\u{1a}\u{1}\u{5}\u{1}\u{1c}\u{1}\0\u{1}\u{b}\u{1}\u{8}\u{1}\u{1b}\u{1}\0",
  "\u{4}\u{b}\u{1}\u{5}\u{3}\0\u{2}\u{b}\u{1}\0\u{3}\u{b}\u{a}\0\u{1}\u{7}\u{6}\0\u{1}\u{23}\u{1}\0\u{1}\u{7}\u{c}\0\u{1}\u{1a}",
  "\u{1}\0\u{1}\u{1a}\u{1}\u{1b}\u{1}\u{1a}\u{1}\u{5}\u{1}\u{1c}\u{1}\0\u{1}\u{d}\u{1}\u{8}\u{1}\u{1b}\u{1}\0\u{1}\u{d}\u{1}\u{5}\u{1}\u{24}",
  "\u{1}\u{d}\u{1}\u{5}\u{3}\0\u{1}\u{5}\u{1}\u{25}\u{1}\0\u{3}\u{d}\u{b}\0\u{1}\u{e}\u{3}\0\u{1}\u{e}\u{1}\0\u{2}\u{e}\u{4}\0",
  "\u{2}\u{e}\u{1}\0\u{3}\u{e}\u{9}\0\u{1}\u{1c}\u{1}\0\u{1}\u{f}\u{3}\0\u{1}\u{f}\u{1}\0\u{2}\u{f}\u{2}\0\u{1}\u{f}\u{1}\0",
  "\u{2}\u{f}\u{1}\0\u{3}\u{f}\u{b}\0\u{1}\u{10}\u{3}\0\u{1}\u{10}\u{1}\0\u{2}\u{10}\u{4}\0\u{2}\u{10}\u{1}\0\u{3}\u{10}\u{b}\0",
  "\u{1}\u{26}\u{3}\0\u{1}\u{26}\u{1}\0\u{2}\u{26}\u{4}\0\u{2}\u{26}\u{1}\u{27}\u{3}\u{26}\u{b}\0\u{1}\u{12}\u{3}\0\u{1}\u{12}\u{1}\0",
  "\u{1}\u{28}\u{1}\u{12}\u{7}\0\u{3}\u{12}\u{b}\0\u{1}\u{14}\u{3}\0\u{1}\u{14}\u{1}\0\u{2}\u{14}\u{5}\0\u{1}\u{15}\u{1}\0\u{3}\u{14}",
  "\u{12}\0\u{1}\u{29}\u{10}\0\u{1}\u{9}\u{4}\0\u{1}\u{16}\u{1}\0\u{1}\u{9}\u{1}\0\u{1}\u{16}\u{1}\0\u{2}\u{16}\u{4}\0\u{2}\u{16}",
  "\u{1}\0\u{3}\u{16}\u{6}\0\u{1}\u{9}\u{1}\0\u{1}\u{5}\u{1}\u{17}\u{1}\0\u{1}\u{17}\u{1}\u{8}\u{1}\u{9}\u{1}\0\u{1}\u{17}\u{1}\u{5}",
  "\u{2}\u{17}\u{1}\u{5}\u{1}\0\u{1}\u{1e}\u{1}\0\u{2}\u{17}\u{1}\0\u{3}\u{17}\u{3}\0\u{3}\u{16}\u{1}\u{9}\u{1}\0\u{1}\u{5}\u{1}\u{17}",
  "\u{1}\0\u{1}\u{18}\u{1}\u{8}\u{1}\u{9}\u{1}\0\u{1}\u{18}\u{1}\u{5}\u{2}\u{18}\u{1}\u{5}\u{3}\0\u{1}\u{9}\u{1}\u{19}\u{1}\0\u{3}\u{18}",
  "\u{3}\0\u{3}\u{16}\u{1}\u{9}\u{1}\0\u{1}\u{5}\u{1}\u{17}\u{1}\0\u{1}\u{9}\u{1}\u{8}\u{1}\u{9}\u{1}\0\u{1}\u{9}\u{1}\u{5}\u{1}\u{9}",
  "\u{1}\u{2a}\u{1}\u{5}\u{3}\0\u{2}\u{9}\u{1}\0\u{3}\u{9}\u{8}\0\u{1}\u{5}\u{2}\0\u{1}\u{1a}\u{1}\u{5}\u{2}\0\u{1}\u{1a}\u{1}\u{5}",
  "\u{2}\u{1a}\u{1}\u{5}\u{3}\0\u{2}\u{1a}\u{1}\0\u{3}\u{1a}\u{3}\0\u{3}\u{2b}\u{1}\u{1b}\u{1}\0\u{1}\u{5}\u{1}\u{1c}\u{1}\0\u{1}\u{1b}",
  "\u{1}\u{8}\u{1}\u{1b}\u{1}\0\u{1}\u{1b}\u{1}\u{5}\u{2}\u{1b}\u{1}\u{5}\u{3}\0\u{2}\u{1b}\u{1}\0\u{3}\u{1b}\u{6}\0\u{1}\u{1b}\u{1}\0",
  "\u{1}\u{5}\u{1}\u{1c}\u{1}\0\u{1}\u{1c}\u{1}\u{8}\u{1}\u{1b}\u{1}\0\u{1}\u{1c}\u{1}\u{5}\u{2}\u{1c}\u{1}\u{5}\u{1}\0\u{1}\u{1e}\u{1}\0",
  "\u{2}\u{1c}\u{1}\0\u{3}\u{1c}\u{9}\0\u{1}\u{1c}\u{1}\0\u{1}\u{1e}\u{3}\0\u{1}\u{1e}\u{1}\0\u{2}\u{1e}\u{2}\0\u{1}\u{1e}\u{1}\0",
  "\u{2}\u{1e}\u{1}\0\u{3}\u{1e}\u{a}\0\u{1}\u{2c}\u{1}\u{7}\u{3}\0\u{1}\u{7}\u{1}\0\u{1}\u{1f}\u{1}\u{7}\u{1}\u{2c}\u{4}\0\u{1}\u{20}",
  "\u{1}\0\u{3}\u{7}\u{11}\0\u{1}\u{2d}\u{9}\0\u{1}\u{2e}\u{c}\0\u{1}\u{21}\u{1}\u{22}\u{2}\0\u{1}\u{21}\u{1}\0\u{2}\u{21}\u{4}\0",
  "\u{2}\u{21}\u{1}\0\u{3}\u{21}\u{6}\0\u{1}\u{1b}\u{1}\0\u{1}\u{5}\u{1}\u{1c}\u{1}\0\u{1}\u{22}\u{1}\u{8}\u{1}\u{1b}\u{1}\0\u{1}\u{22}",
  "\u{1}\u{5}\u{2}\u{22}\u{1}\u{5}\u{3}\0\u{2}\u{22}\u{1}\0\u{3}\u{22}\u{3}\0\u{1}\u{1a}\u{1}\0\u{1}\u{1a}\u{1}\u{1b}\u{1}\u{1a}\u{1}\u{5}",
  "\u{1}\u{1c}\u{1}\u{2c}\u{1}\u{d}\u{1}\u{8}\u{1}\u{1b}\u{1}\0\u{1}\u{d}\u{1}\u{5}\u{1}\u{24}\u{1}\u{d}\u{1}\u{2f}\u{3}\0\u{1}\u{5}\u{1}\u{25}",
  "\u{1}\0\u{3}\u{d}\u{3}\0\u{1}\u{1a}\u{1}\0\u{1}\u{1a}\u{1}\u{1b}\u{1}\u{1a}\u{1}\u{5}\u{1}\u{1c}\u{1}\0\u{1}\u{5}\u{1}\u{8}\u{1}\u{1b}",
  "\u{1}\0\u{2}\u{5}\u{1}\u{30}\u{2}\u{5}\u{3}\0\u{2}\u{5}\u{1}\0\u{1}\u{5}\u{1}\u{31}\u{1}\u{5}\u{b}\0\u{1}\u{27}\u{3}\0\u{1}\u{27}",
  "\u{1}\0\u{2}\u{27}\u{4}\0\u{2}\u{27}\u{1}\0\u{3}\u{27}\u{a}\0\u{1}\u{2c}\u{1}\u{12}\u{3}\0\u{1}\u{12}\u{1}\0\u{1}\u{28}\u{1}\u{12}",
  "\u{1}\u{2c}\u{6}\0\u{3}\u{12}\u{b}\0\u{1}\u{29}\u{3}\0\u{1}\u{29}\u{1}\0\u{2}\u{29}\u{7}\0\u{3}\u{29}\u{3}\0\u{3}\u{16}\u{1}\u{9}",
  "\u{1}\0\u{1}\u{5}\u{1}\u{17}\u{1}\0\u{1}\u{2a}\u{1}\u{8}\u{1}\u{9}\u{1}\0\u{1}\u{2a}\u{1}\u{5}\u{2}\u{2a}\u{1}\u{5}\u{3}\0\u{2}\u{9}",
  "\u{1}\0\u{3}\u{2a}\u{6}\0\u{1}\u{1b}\u{4}\0\u{1}\u{2b}\u{1}\0\u{1}\u{1b}\u{1}\0\u{1}\u{2b}\u{1}\0\u{2}\u{2b}\u{4}\0\u{2}\u{2b}",
  "\u{1}\0\u{3}\u{2b}\u{b}\0\u{1}\u{2c}\u{3}\0\u{1}\u{2c}\u{1}\0\u{1}\u{32}\u{1}\u{2c}\u{5}\0\u{1}\u{33}\u{1}\0\u{3}\u{2c}\u{a}\0",
  "\u{1}\u{2c}\u{6}\0\u{1}\u{34}\u{1}\0\u{1}\u{2c}\u{6}\0\u{1}\u{12}\u{1d}\0\u{1}\u{2e}\u{1}\u{35}\u{3}\0\u{1}\u{1a}\u{1}\0\u{1}\u{1a}",
  "\u{1}\u{1b}\u{1}\u{1a}\u{1}\u{5}\u{1}\u{1c}\u{1}\0\u{1}\u{2f}\u{1}\u{8}\u{1}\u{1b}\u{1}\0\u{1}\u{2f}\u{1}\u{5}\u{1}\u{36}\u{1}\u{2f}\u{1}\u{5}",
  "\u{3}\0\u{1}\u{5}\u{1}\u{37}\u{1}\0\u{3}\u{2f}\u{3}\0\u{1}\u{1a}\u{1}\0\u{1}\u{1a}\u{1}\u{1b}\u{1}\u{1a}\u{1}\u{5}\u{1}\u{1c}\u{1}\u{2c}",
  "\u{1}\u{5}\u{1}\u{8}\u{1}\u{1b}\u{1}\0\u{2}\u{5}\u{1}\u{38}\u{1}\u{5}\u{1}\u{2f}\u{3}\0\u{2}\u{5}\u{1}\0\u{1}\u{39}\u{2}\u{5}\u{3}\0",
  "\u{1}\u{1a}\u{1}\0\u{1}\u{1a}\u{1}\u{1b}\u{1}\u{1a}\u{1}\u{5}\u{1}\u{1c}\u{1}\0\u{1}\u{5}\u{1}\u{8}\u{1}\u{1b}\u{1}\0\u{5}\u{5}\u{3}\0",
  "\u{2}\u{5}\u{1}\0\u{1}\u{5}\u{1}\u{31}\u{1}\u{3a}\u{a}\0\u{2}\u{2c}\u{3}\0\u{1}\u{2c}\u{1}\0\u{1}\u{32}\u{2}\u{2c}\u{4}\0\u{1}\u{33}",
  "\u{1}\0\u{3}\u{2c}\u{11}\0\u{1}\u{2d}\u{15}\0\u{1}\u{2c}\u{6}\0\u{1}\u{34}\u{1}\0\u{1}\u{2c}\u{c}\0\u{1}\u{1a}\u{1}\0\u{1}\u{1a}",
  "\u{1}\u{1b}\u{1}\u{1a}\u{1}\u{5}\u{1}\u{1c}\u{1}\u{2c}\u{1}\u{2f}\u{1}\u{8}\u{1}\u{1b}\u{1}\0\u{1}\u{2f}\u{1}\u{5}\u{1}\u{36}\u{2}\u{2f}\u{3}\0",
  "\u{1}\u{5}\u{1}\u{37}\u{1}\0\u{3}\u{2f}\u{3}\0\u{1}\u{1a}\u{1}\0\u{1}\u{1a}\u{1}\u{1b}\u{1}\u{1a}\u{1}\u{5}\u{1}\u{1c}\u{1}\0\u{1}\u{5}",
  "\u{1}\u{8}\u{1}\u{1b}\u{1}\0\u{2}\u{5}\u{1}\u{30}\u{2}\u{5}\u{3}\0\u{2}\u{5}\u{1}\0\u{3}\u{5}\u{3}\0\u{1}\u{1a}\u{1}\0\u{1}\u{1a}",
  "\u{1}\u{1b}\u{1}\u{1a}\u{1}\u{5}\u{1}\u{1c}\u{1}\u{2c}\u{1}\u{5}\u{1}\u{8}\u{1}\u{1b}\u{1}\0\u{2}\u{5}\u{1}\u{38}\u{1}\u{5}\u{1}\u{2f}\u{3}\0",
  "\u{2}\u{5}\u{1}\0\u{3}\u{5}\u{3}\0\u{1}\u{1a}\u{1}\0\u{1}\u{1a}\u{1}\u{1b}\u{1}\u{1a}\u{1}\u{5}\u{1}\u{1c}\u{1}\0\u{1}\u{39}\u{1}\u{8}",
  "\u{1}\u{1b}\u{1}\0\u{1}\u{39}\u{1}\u{5}\u{1}\u{3b}\u{1}\u{39}\u{1}\u{5}\u{3}\0\u{2}\u{5}\u{1}\0\u{3}\u{39}\u{3}\0\u{1}\u{1a}\u{1}\0",
  "\u{1}\u{1a}\u{1}\u{1b}\u{1}\u{1a}\u{1}\u{5}\u{1}\u{1c}\u{1}\u{2c}\u{1}\u{39}\u{1}\u{8}\u{1}\u{1b}\u{1}\0\u{1}\u{39}\u{1}\u{5}\u{1}\u{3b}\u{1}\u{39}",
  "\u{1}\u{2f}\u{3}\0\u{2}\u{5}\u{1}\0\u{3}\u{39}",
);
fn zz_unpack_trans() -> Vec<[i32; ZZ_TRANS_WIDTH]> {
  let mut result = vec![0; 1537];
  zz_unpack_trans_with_offset(ZZ_TRANS_PACKED_0, 0, &mut result);
  ZZ_ROWMAP
    .iter()
    .map(|&offset| {
      let mut row = [-1; ZZ_TRANS_WIDTH];
      row[..29].copy_from_slice(&result[offset..offset + 29]);
      row
    })
    .collect()
}
fn zz_unpack_trans_with_offset(packed: &str, offset: usize, result: &mut [i32]) -> usize {
  let mut packed = packed.chars();
  let mut j = offset;
  while let Some(count) = packed.next() {
    let mut count = count as usize;
    let mut value = expect_invariant!(
      packed.next(),
      "packed tokenizer table has count/value pairs",
    ) as i32;
    value -= 1;
    while count > 0 {
      result[j] = value;
      j += 1;
      count -= 1;
    }
  }
  j
}
/// Error code for "Unknown internal scanner error"
const ZZ_UNKNOWN_ERROR: usize = 0;
/// Error code for "could not match input"
const ZZ_NO_MATCH: usize = 1;
/// Error code for "pushback value was too large"
const ZZ_PUSHBACK_2BIG: usize = 2;
/// Error messages for ZZ_UNKNOWN_ERROR, ZZ_NO_MATCH, and ZZ_PUSHBACK_2BIG respectively.
const ZZ_ERROR_MSG: [&str; 3] = [
  "Unknown internal scanner error",
  "Error: could not match input",
  "Error: pushback value was too large",
];
/// ZZ_ATTRIBUTE[aState] contains the attributes of state aState
static ZZ_ATTRIBUTE: LazyLock<[i32; 59]> = LazyLock::new(zz_unpack_attribute);
const ZZ_ATTRIBUTE_PACKED_0: &str = "\u{1}\0\u{1}\u{9}\u{10}\u{1}\u{1}\0\u{1}\u{1}\u{2}\0\u{3}\u{1}\u{1}\0\
\u{2}\u{1}\u{1}\0\u{3}\u{1}\u{1}\0\u{1}\u{1}\u{1}\0\u{2}\u{1}\u{1}\0\
\u{4}\u{1}\u{1}\0\u{1}\u{1}\u{2}\0\u{5}\u{1}\u{1}\0\u{1}\u{9}\u{6}\u{1}";

fn zz_unpack_attribute() -> [i32; 59] {
  let mut result = [0; 59];
  zz_unpack_attribute_with_offset(ZZ_ATTRIBUTE_PACKED_0, 0, &mut result);
  result
}
fn zz_unpack_attribute_with_offset(packed: &str, offset: usize, result: &mut [i32]) -> usize {
  let mut packed = packed.chars();
  let mut j = offset;
  while let Some(count) = packed.next() {
    let mut count = count as usize;
    let value = expect_invariant!(
      packed.next(),
      "packed tokenizer attributes have count/value pairs"
    ) as i32;
    while count > 0 {
      result[j] = value;
      j += 1;
      count -= 1;
    }
  }
  j
}
const ZZ_LEXSTATE: [usize; 2] = [0, 0];
/// This character denotes the end of file
pub const YYEOF: i32 = -1;

/// Lexical States
pub const YYINITIAL: i32 = 0;
#[derive(Debug, Clone)]
pub struct StandardTokenizerImpl {
  zz_reader: ReaderEnum,
  zz_buffer: Vec<char>,
  zz_current_pos: usize,
  zz_marked_pos: usize,
  zz_start_read: usize,
  zz_end_read: usize,
  zz_end_read_units: usize,
  zz_buffered_end: usize,
  yychar: usize,
  pub(crate) match_length_units: usize,
  zz_buffer_size: usize,
  lexical_state: i32,
  at_eof: bool,
}

impl StandardTokenizerImpl {
  pub fn new<R>(input: R) -> Self
  where
    R: Into<ReaderEnum>,
  {
    let zz_buffer_size = 255;
    Self {
      zz_reader: input.into(),
      zz_buffer: vec!['\0'; zz_buffer_size],
      zz_current_pos: 0,
      zz_marked_pos: 0,
      zz_start_read: 0,
      zz_end_read: 0,
      zz_end_read_units: 0,
      zz_buffered_end: 0,
      yychar: 0,
      match_length_units: 0,
      zz_buffer_size,
      lexical_state: YYINITIAL,
      at_eof: false,
    }
  }

  /// Alphanumeric sequences
  pub const WORD_TYPE: i32 = ALPHANUM;
  /// Numbers
  pub const NUMERIC_TYPE: i32 = NUM;
  /// Chars in class `\p{Line_Break = Complex_Context}` are from South East Asian
  /// scripts (Thai, Lao, Myanmar, Khmer, etc.). Sequences of these are kept
  /// together as as a single token rather than broken up, because the logic
  /// required to break them at word boundaries is too complex for UAX#29.
  ///
  /// See Unicode Line Breaking Algorithm: <http://www.unicode.org/reports/tr14/#SA>
  pub const SOUTH_EAST_ASIAN_TYPE: i32 = SOUTHEAST_ASIAN;
  /// Ideographic token type
  pub const IDEOGRAPHIC_TYPE: i32 = IDEOGRAPHIC;
  /// Hiragana token type
  pub const HIRAGANA_TYPE: i32 = HIRAGANA;
  /// Katakana token type
  pub const KATAKANA_TYPE: i32 = KATAKANA;
  /// Hangul token type
  pub const HANGUL_TYPE: i32 = HANGUL;
  /// Emoji token type
  pub const EMOJI_TYPE: i32 = EMOJI;
  /// Number of UTF-16 units before the current match.
  pub fn yychar(&self) -> i32 {
    self.yychar as i32
  }
  /// Fills CharTermAttribute with the current token text
  pub fn get_text<T>(&self, t: &mut T) -> Result<()>
  where
    T: AttributeSource,
  {
    t.copy_buffer(
      &self.zz_buffer[self.zz_start_read..self.zz_marked_pos],
      0,
      self.zz_marked_pos - self.zz_start_read,
    )
  }
  /// Sets the scanner window size in UTF-16 units.
  pub fn set_buffer_size(&mut self, num_chars: usize) {
    self.zz_buffer_size = num_chars;
    if num_chars >= self.zz_buffer.len() {
      self.zz_buffer.resize(num_chars, '\0');
    } else {
      self.zz_buffer.truncate(num_chars);
      self.zz_buffer.shrink_to_fit();
    }
  }
  /// Translates raw input code points to DFA table row
  fn zz_cmap(input: i32, top: &[i32], blocks: &[u8]) -> u8 {
    let input = input as usize;
    let offset = input & 255;
    if offset == input {
      blocks[offset]
    } else {
      blocks[top[input >> 8] as usize | offset]
    }
  }
  /// Refills the input buffer.
  ///
  /// Returns `false` iff there was new input.
  ///
  /// # Errors
  ///
  /// Returns an error if any I/O error occurs.
  #[inline(never)]
  fn zz_refill(&mut self) -> Result<bool> {
    if self.zz_start_read > 0 {
      let mut consumed = self.zz_start_read;
      if self.zz_end_read_units != self.zz_end_read {
        consumed += self.zz_buffer[..self.zz_start_read]
          .iter()
          .filter(|&&ch| ch.len_utf16() == 2)
          .count();
      }
      self
        .zz_buffer
        .copy_within(self.zz_start_read..self.zz_buffered_end, 0);
      self.zz_end_read_units -= consumed;
      self.zz_end_read -= self.zz_start_read;
      self.zz_buffered_end -= self.zz_start_read;
      self.zz_current_pos -= self.zz_start_read;
      self.zz_marked_pos -= self.zz_start_read;
      self.zz_start_read = 0;
    }

    // Keep the same scan-window limit as JFlex. A Rust reader returns whole
    // scalars, so any read-ahead beyond the window stays in the buffer for the
    // next refill, including a supplementary scalar that straddles the limit.
    let mut read_from_reader = false;
    loop {
      let requested = self.zz_buffer_size.saturating_sub(self.zz_end_read_units);
      if requested == 0 {
        break;
      }
      let known_units = if self.zz_end_read == self.zz_buffered_end {
        if read_from_reader {
          break;
        }
        let (num_read, units) = match &mut self.zz_reader {
          ReaderEnum::String(reader) => {
            let (read, units) = reader.read_range_with_units::<true>(
              &mut self.zz_buffer,
              self.zz_end_read,
              requested,
            )?;
            (read, Some(units))
          },
          ReaderEnum::SharedText(reader) => {
            let (read, units) = reader.read_range_with_units::<true>(
              &mut self.zz_buffer,
              self.zz_end_read,
              requested,
            )?;
            (read, Some(units))
          },
          reader => (
            reader.read_range(&mut self.zz_buffer, self.zz_end_read, requested)?,
            None,
          ),
        };
        if num_read == 0 {
          return Err(LuceneError::io(Error::other(
            "Reader returned 0 characters. See JFlex examples/zero-reader for a workaround.",
          )));
        }
        if num_read == -1 {
          break;
        }
        self.zz_buffered_end += num_read as usize;
        read_from_reader = true;
        units
      } else {
        None
      };

      let available = &self.zz_buffer[self.zz_end_read..self.zz_buffered_end];
      let units = known_units.unwrap_or_else(|| {
        available.len() + available.iter().filter(|&&ch| ch.len_utf16() == 2).count()
      });
      if units <= requested {
        self.zz_end_read = self.zz_buffered_end;
        self.zz_end_read_units += units;
      } else {
        for ch in available {
          let units = ch.len_utf16();
          if units > self.zz_buffer_size - self.zz_end_read_units {
            break;
          }
          self.zz_end_read += 1;
          self.zz_end_read_units += units;
        }
        break;
      }
    }
    Ok(self.zz_current_pos == self.zz_end_read)
  }
  /// Closes the input reader.
  pub fn yyclose(&mut self) -> Result<()> {
    self.at_eof = true;
    if self.zz_end_read_units != self.zz_end_read {
      self.zz_end_read_units = self.zz_start_read
        + self.zz_buffer[..self.zz_start_read]
          .iter()
          .filter(|&&ch| ch.len_utf16() == 2)
          .count();
    } else {
      self.zz_end_read_units = self.zz_start_read;
    }
    self.zz_end_read = self.zz_start_read;
    self.zz_buffered_end = self.zz_start_read;
    self.zz_reader.close()
  }
  /// Resets the scanner to read from a new input stream.
  ///
  /// Does not close the old reader.
  ///
  /// All internal variables are reset, and the old input stream **cannot** be reused
  /// (the internal buffer is discarded and lost). Lexical state is set to `ZZ_INITIAL`.
  ///
  /// The internal scan buffer is resized back to its initial length if it has grown.
  ///
  /// # Parameters
  ///
  /// * `reader` - The new input stream.
  pub fn yyreset<R>(&mut self, input: R)
  where
    R: Into<ReaderEnum>,
  {
    self.zz_reader = input.into();
    self.yy_reset_position();
    self.lexical_state = YYINITIAL;
  }
  /// Resets the input position.
  fn yy_reset_position(&mut self) {
    self.at_eof = false;
    self.zz_current_pos = 0;
    self.zz_marked_pos = 0;
    self.zz_start_read = 0;
    self.zz_end_read = 0;
    self.zz_end_read_units = 0;
    self.zz_buffered_end = 0;
    self.yychar = 0;
    self.match_length_units = 0;
  }

  /// Returns whether the scanner has reached the end of the reader it reads from.
  ///
  /// Returns whether the scanner has reached EOF.
  pub fn yyat_eof(&self) -> bool {
    self.at_eof
  }
  /// Returns the current lexical state.
  pub fn yystate(&self) -> i32 {
    self.lexical_state
  }
  /// Enters a new lexical state.
  ///
  /// # Parameters
  ///
  /// * `new_state` - The new lexical state.
  pub fn yybegin(&mut self, new_state: i32) {
    self.lexical_state = new_state;
  }
  /// Returns the text matched by the current regular expression.
  pub fn yytext(&self) -> String {
    self.zz_buffer[self.zz_start_read..self.zz_marked_pos]
      .iter()
      .collect()
  }
  /// Returns the character at the given position from the matched text.
  ///
  /// Equivalent to `yytext().chars().nth(position)`, but faster.
  ///
  /// # Parameters
  ///
  /// * `position` - The position of the character to fetch. A value from `0` to
  ///   `yylength() - 1`.
  ///
  /// # Returns
  ///
  /// The character at `position`.
  pub fn yycharat(&self, position: usize) -> char {
    self.zz_buffer[self.zz_start_read + position]
  }
  /// Returns the length of the matched text region.
  pub fn yylength(&self) -> usize {
    self.zz_marked_pos - self.zz_start_read
  }

  /// Reports an error that occurred while scanning.
  ///
  /// In a well-formed scanner, with no misuse of `yypushback(i32)` and with a
  /// match-all fallback rule, this method should only be called for cases that
  /// "can't possibly happen".
  ///
  /// If this method is called, something is seriously wrong, such as a JFlex bug
  /// producing a faulty scanner.
  ///
  /// Normal syntax or scanner-level error handling should be implemented in error
  /// fallback rules.
  ///
  /// # Parameters
  ///
  /// * `error_code` - The code of the error message to display.
  fn zz_scan_error(error_code: usize) -> ! {
    let message = ZZ_ERROR_MSG
      .get(error_code)
      .unwrap_or(&ZZ_ERROR_MSG[ZZ_UNKNOWN_ERROR]);
    panic_invariant!(
      "scanner errors indicate a faulty DFA or a violated yypushback contract",
      "{message}"
    )
  }
  /// Pushes the specified number of characters back into the input stream.
  ///
  /// They will be read again by the next call of the scanning method.
  ///
  /// # Parameters
  ///
  /// * `number` - The number of characters to be read again. This number must not
  ///   be greater than `yylength()`.
  pub fn yypushback(&mut self, number: usize) {
    if number > self.yylength() {
      Self::zz_scan_error(ZZ_PUSHBACK_2BIG);
    }
    let end = self.zz_marked_pos;
    self.zz_marked_pos -= number;
    self.match_length_units -= self.zz_buffer[self.zz_marked_pos..end]
      .iter()
      .map(|ch| ch.len_utf16())
      .sum::<usize>();
  }
  /// Resumes scanning until the next regular expression is matched, the end of
  /// input is reached, or an I/O error occurs.
  ///
  /// Returns the next token.
  ///
  /// # Errors
  ///
  /// Returns an error if any I/O error occurs.
  pub fn get_next_token(&mut self) -> Result<i32> {
    let zz_trans = ZZ_TRANS.as_slice();
    let zz_attr = ZZ_ATTRIBUTE.as_slice();
    let zz_cmap_top = ZZ_CMAP_TOP.as_slice();
    let zz_cmap_blocks = ZZ_CMAP_BLOCKS.as_slice();
    loop {
      let zz_marked_pos = self.zz_marked_pos;
      self.yychar += self.match_length_units;
      let mut zz_action = None;

      let mut zz_current_pos_l = zz_marked_pos;
      let mut zz_marked_pos_l = zz_marked_pos;
      self.zz_current_pos = zz_current_pos_l;
      self.zz_start_read = zz_marked_pos_l;
      let mut zz_state = ZZ_LEXSTATE[self.lexical_state as usize];

      let mut zz_attributes = zz_attr[zz_state];
      if (zz_attributes & 1) == 1 {
        zz_action = Some(zz_state);
      }

      let zz_input = loop {
        let zz_input;
        if zz_current_pos_l < self.zz_end_read {
          zz_input = self.zz_buffer[zz_current_pos_l] as i32;
          zz_current_pos_l += 1;
        } else if self.at_eof {
          zz_input = YYEOF;
          break zz_input;
        } else {
          self.zz_current_pos = zz_current_pos_l;
          self.zz_marked_pos = zz_marked_pos_l;
          let eof = self.zz_refill()?;
          zz_current_pos_l = self.zz_current_pos;
          zz_marked_pos_l = self.zz_marked_pos;
          if eof {
            zz_input = YYEOF;
            break zz_input;
          }
          zz_input = self.zz_buffer[zz_current_pos_l] as i32;
          zz_current_pos_l += 1;
        }
        let zz_next =
          zz_trans[zz_state][Self::zz_cmap(zz_input, zz_cmap_top, zz_cmap_blocks) as usize];
        if zz_next == -1 {
          break zz_input;
        }
        zz_state = zz_next as usize;

        zz_attributes = zz_attr[zz_state];
        if (zz_attributes & 1) == 1 {
          zz_action = Some(zz_state);
          zz_marked_pos_l = zz_current_pos_l;
          if (zz_attributes & 8) == 8 {
            break zz_input;
          }
        }
      };

      self.zz_marked_pos = zz_marked_pos_l;
      // Keep the matched length in Lucene offset units for both the caller
      // and the next scan. A BMP-only window needs no second pass.
      self.match_length_units = self.yylength();
      if self.zz_end_read_units != self.zz_end_read {
        self.match_length_units += self.zz_buffer[self.zz_start_read..zz_marked_pos_l]
          .iter()
          .filter(|&&ch| ch.len_utf16() == 2)
          .count();
      }

      if zz_input == YYEOF && self.zz_start_read == self.zz_current_pos {
        self.at_eof = true;
        return Ok(YYEOF);
      }

      match zz_action.map_or(-1, |state| ZZ_ACTION[state]) {
        1 | 10 | 11 | 12 | 13 | 14 | 15 | 16 | 17 | 18 => {},
        2 => return Ok(Self::NUMERIC_TYPE),
        3 => return Ok(Self::WORD_TYPE),
        4 => return Ok(Self::EMOJI_TYPE),
        5 => return Ok(Self::SOUTH_EAST_ASIAN_TYPE),
        6 => return Ok(Self::HANGUL_TYPE),
        7 => return Ok(Self::IDEOGRAPHIC_TYPE),
        8 => return Ok(Self::KATAKANA_TYPE),
        9 => return Ok(Self::HIRAGANA_TYPE),
        _ => Self::zz_scan_error(ZZ_NO_MATCH),
      }
    }
  }
}
