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
use crate::core::util::error::lucene_error::LuceneError;
use crate::core::util::error::lucene_error::Result;

pub struct UnicodeUtil;
impl UnicodeUtil {
    pub(crate) const UNI_SUR_HIGH_START: i32 = 0xD800;
    pub(crate) const UNI_SUR_HIGH_END: i32 = 0xDBFF;
    pub(crate) const UNI_SUR_LOW_START: i32 = 0xDC00;
    pub(crate) const UNI_SUR_LOW_END: i32 = 0xDFFF;
    pub(crate) const MAX_UTF8_BYTES_PER_CHAR: i32 = 3;
    pub fn valid_utf16_string(s: &str) -> bool {
        let utf16: Vec<u16> = s.encode_utf16().collect();
        let mut i = 0;
        while i < utf16.len() {
            let ch = utf16[i] as i32;
            if (Self::UNI_SUR_HIGH_START..=Self::UNI_SUR_HIGH_END).contains(&ch) {
                // High surrogate
                if i + 1 < utf16.len() {
                    let next = utf16[i + 1] as i32;
                    if (Self::UNI_SUR_LOW_START..=Self::UNI_SUR_LOW_END).contains(&next) {
                        // Valid surrogate pair
                        i += 2;
                        continue;
                    } else {
                        // Unmatched high surrogate
                        return false;
                    }
                } else {
                    // Trailing high surrogate
                    return false;
                }
            } else if (Self::UNI_SUR_LOW_START..=Self::UNI_SUR_LOW_END).contains(&ch) {
                // Unmatched low surrogate
                return false;
            }
            i += 1;
        }
        true
    }

    pub(crate) fn code_point_at<'a>(
        utf8: &[u8],
        pos: usize,
        reuse: &'a mut UTF8CodePoint,
    ) -> Result<&'a mut UTF8CodePoint> {
        if pos >= utf8.len() {
            return Err(LuceneError::illegal_argument(format!(
                "Position {} out of bounds for utf8 array of length {}",
                pos,
                utf8.len()
            )));
        }

        let lead_byte = utf8[pos] as usize;
        let num_bytes = UTF8_CODE_LENGTH[lead_byte];

        if num_bytes == i32::MIN {
            return Err(LuceneError::illegal_argument(format!(
                "Invalid UTF8 header byte: 0x{lead_byte:X}"
            )));
        }

        reuse.num_bytes = num_bytes as usize;

        let mut v: i32;
        match num_bytes {
            1 => {
                reuse.code_point = lead_byte as i32;
                return Ok(reuse);
            },
            2 => v = (lead_byte & 0b0001_1111) as i32,
            3 => v = (lead_byte & 0b0000_1111) as i32,
            4 => v = (lead_byte & 0b0000_0111) as i32,
            _ => {
                return Err(LuceneError::illegal_argument(format!(
                    "Invalid UTF8 header byte: 0x{lead_byte:X}"
                )));
            },
        }

        let limit = pos + reuse.num_bytes;
        let mut i = pos + 1;
        while i < limit {
            if i >= utf8.len() {
                return Err(LuceneError::illegal_argument("UTF-8 sequence truncated"));
            }
            v = (v << 6) | ((utf8[i] & 0b0011_1111) as i32);
            i += 1;
        }
        reuse.code_point = v;

        Ok(reuse)
    }
    pub fn new_string(code_points: &[i32], offset: usize, count: usize) -> Result<String> {
        if offset + count > code_points.len() {
            return Err(LuceneError::illegal_argument(
                "offset + count out of bounds",
            ));
        }

        let mut result = String::with_capacity(count);

        for &cp in &code_points[offset..offset + count] {
            if !(0..=0x10FFFF).contains(&cp) {
                return Err(LuceneError::illegal_argument(format!(
                    "Invalid code point: {cp}"
                )));
            }

            if let Some(c) = std::char::from_u32(cp as u32) {
                result.push(c);
            } else {
                return Err(LuceneError::illegal_argument(format!(
                    "Invalid Unicode scalar value: {cp}"
                )));
            }
        }

        Ok(result)
    }
    /// Returns the maximum number of utf8 bytes required to encode
    pub fn max_utf8_length(code: i32) -> Result<usize> {
        match code {
            0x0000..=0x007F => Ok(1),    // ASCII
            0x0080..=0x07FF => Ok(2),    // 2-byte UTF-8
            0x0800..=0xFFFF => Ok(3),    // 3-byte UTF-8
            0x10000..=0x10FFFF => Ok(4), // 4-byte UTF-8
            _ => Err(LuceneError::illegal_argument("Invalid Unicode code point")),
        }
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct UTF8CodePoint {
    pub code_point: i32,
    pub num_bytes: usize,
}
pub(crate) static UTF8_CODE_LENGTH: [i32; 248] = {
    const V: i32 = i32::MIN;
    [
        1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, // 0x00
        1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, // 0x10
        1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, // 0x20
        1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, // 0x30
        1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, // 0x40
        1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, // 0x50
        1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, // 0x60
        1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, // 0x70
        V, V, V, V, V, V, V, V, V, V, V, V, V, V, V, V, // 0x80
        V, V, V, V, V, V, V, V, V, V, V, V, V, V, V, V, // 0x90
        V, V, V, V, V, V, V, V, V, V, V, V, V, V, V, V, // 0xA0
        V, V, V, V, V, V, V, V, V, V, V, V, V, V, V, V, // 0xB0
        2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, // 0xC0
        2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, // 0xD0
        3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, // 0xE0
        4, 4, 4, 4, 4, 4, 4, 4,
    ]
};
