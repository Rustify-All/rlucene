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

use crate::core::index::{BytesRef, BytesRefBuilder};
use crate::core::util::access::{ByteSource, SharedAccessVec};

pub struct ToStringUtils;

impl ToStringUtils {
  pub fn long_hex(x: u64) -> String {
    format!("0x{x:016x}")
  }

  pub fn bytes_ref_to_string<AV>(b: &BytesRef<AV>) -> String
  where
    AV: SharedAccessVec<u8> + ByteSource,
  {
    b.bytes.access(|bytes| {
      if bytes.is_empty() {
        return "null".to_string();
      }
      let v = b.bytes.access(|bytes| {
        b.offset
          .checked_add(b.length)
          .and_then(|end| bytes.get(b.offset..end))
          .and_then(|bytes| std::str::from_utf8(bytes).ok())
          .map(str::to_owned)
      });
      match v {
        Some(s) => s,
        None => {
          // If BytesRef isn't actually UTF-8, or it's e.g. a prefix of UTF-8
          // that ends mid-unicode-char, we fall back to hex:
          b.to_string()
        },
      }
    })
  }

  pub fn bytes_ref_to_string_from_builder<AV>(b: &BytesRefBuilder<AV>) -> String
  where
    AV: SharedAccessVec<u8> + ByteSource,
  {
    Self::bytes_ref_to_string(b.get_bytes_ref())
  }
  pub fn bytes_ref_to_string_from_bytes<B>(b: B) -> String
  where
    B: Into<Vec<u8>>,
  {
    Self::bytes_ref_to_string(&BytesRef::from_bytes(b.into()))
  }
}
