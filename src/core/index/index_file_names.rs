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
use crate::core::util::error::lucene_error::Result;
use num_bigint::BigInt;

/// This struct contains useful constants representing filenames and extensions
/// used by Lucene, as well as convenience methods for querying whether a file
/// name matches an extension
/// ([`matches_extension`](IndexFileNames::matches_extension)), as well as
/// generating file names from a segment name, generation, and extension
/// ([`file_name_from_generation`](IndexFileNames::file_name_from_generation),
/// [`segment_file_name`](IndexFileNames::segment_file_name)).
///
/// # Note
/// Extensions used by codecs are not listed here. You must interact with the
/// [`Codec`](crate::core::codecs::Codec) directly.
///
/// # Note
/// This is an internal API.
pub struct IndexFileNames;

impl IndexFileNames {
  /// Name of the index segment file
  pub const SEGMENTS: &'static str = "segments";
  /// Name of a pending index segment file
  pub const PENDING_SEGMENTS: &'static str = "pending_segments";
  /// Computes the full file name from `base`, `extension`, and `generation`.
  /// If the generation is `-1`, the file name is `None`. If it's `0`, the
  /// file name is `<base>.<ext>`. If it's greater than `0`, the file name
  /// is `<base>_<gen>.<ext>`.
  ///
  /// # Note
  /// `.ext` is added to the name only if `ext` is not an empty string.
  ///
  /// # Arguments
  /// * `base` - Main part of the file name.
  /// * `ext` - Extension of the filename.
  /// * `gen` - Generation.
  pub fn file_name_from_generation(base: &str, ext: &str, gen_: i64) -> Option<String> {
    if gen_ == -1 {
      return None;
    }
    if gen_ == 0 {
      Option::from(IndexFileNames::segment_file_name(base, "", ext))
    } else {
      // base-36
      let gen_str = BigInt::from(gen_).to_str_radix(36);
      let mut res = String::with_capacity(base.len() + 6 + ext.len());
      res.push_str(base);
      res.push('_');
      res.push_str(&gen_str);

      if !ext.is_empty() {
        res.push('.');
        res.push_str(ext);
      }
      Option::from(res)
    }
  }
  /// Returns a file name that includes the given `segment_name`, your own
  /// custom `name`, and `extension`. The format of the filename is:
  /// `<segment_name>_<name>.<ext>`.
  ///
  /// # Note
  /// - `.ext` is added to the result file name only if `ext` is not empty.
  /// - `_segment_suffix` is added to the result file name only if it's not
  ///   the empty string.
  /// - All custom files should be named using this method, or otherwise some
  ///   structures may fail to handle them properly (such as if they are added
  ///   to compound files).
  ///
  /// # Arguments
  /// * `segment_name` - The segment name.
  /// * `name` - The custom name.
  /// * `ext` - The file extension.
  pub fn segment_file_name(segment_name: &str, segment_suffix: &str, ext: &str) -> String {
    if !ext.is_empty() || !segment_suffix.is_empty() {
      debug_assert!(!ext.starts_with('.'), "Extension should not start with '.'");
      let mut sb = String::with_capacity(segment_name.len() + 2 + segment_suffix.len() + ext.len());
      sb.push_str(segment_name);
      if !segment_suffix.is_empty() {
        sb.push('_');
        sb.push_str(segment_suffix);
      }
      if !ext.is_empty() {
        sb.push('.');
        sb.push_str(ext);
      }
      sb
    } else {
      segment_name.to_string()
    }
  }

  pub(crate) fn with_segment_file_name<R>(
    segment_name: &str,
    suffix: &str,
    ext: &str,
    use_name: impl FnOnce(&str) -> R,
  ) -> R {
    if suffix.is_empty() && ext.is_empty() {
      return use_name(segment_name);
    }
    debug_assert!(!ext.starts_with('.'), "Extension should not start with '.'");
    let mut name = smallvec::SmallVec::<[u8; 64]>::with_capacity(
      segment_name.len()
        + suffix.len()
        + ext.len()
        + usize::from(!suffix.is_empty())
        + usize::from(!ext.is_empty()),
    );
    name.extend_from_slice(segment_name.as_bytes());
    if !suffix.is_empty() {
      name.push(b'_');
      name.extend_from_slice(suffix.as_bytes());
    }
    if !ext.is_empty() {
      name.push(b'.');
      name.extend_from_slice(ext.as_bytes());
    }
    let name = expect_invariant!(
      std::str::from_utf8(&name),
      "filename parts and separators are UTF-8"
    );
    use_name(name)
  }

  /// Returns true if the given filename ends with the given extension. One
  /// should provide a `pure` extension, without '.'.
  pub fn matches_extension(filename: &str, ext: &str) -> bool {
    filename
      .strip_suffix(ext)
      .is_some_and(|prefix| prefix.ends_with('.'))
  }
  /// Locates the boundary of the segment name, if present.
  pub fn index_of_segment_name(filename: &str) -> Option<usize> {
    debug_assert!(filename.len() <= i32::MAX as usize);
    // ASCII '_' cannot occur inside a multi-byte UTF-8 character.
    if let Some(idx) = filename
      .as_bytes()
      .iter()
      .skip(1)
      .position(|&byte| byte == b'_')
    {
      Some(idx + 1)
    } else {
      filename.find('.')
    }
  }

  /// Strips the segment name out of the given file name. If you used
  /// [`segment_file_name`](Self::segment_file_name) or
  /// [`file_name_from_generation`](Self::file_name_from_generation) to
  /// create your files, this method simply removes whatever comes before
  /// the first `.` or the second `_` (excluding both).
  ///
  /// # Returns
  /// The filename with the segment name removed, or the given filename if it
  /// does not contain a `.` and `_`.
  pub fn strip_segment_name(filename: &str) -> &str {
    if let Some(idx) = IndexFileNames::index_of_segment_name(filename) {
      &filename[idx..]
    } else {
      filename
    }
  }

  /// Returns the generation from this file name, or 0 if there is no
  /// generation.
  pub fn parse_generation(filename: &str) -> Result<i64> {
    debug_assert!(filename.starts_with('_'), "Filename must start with '_'");

    let stripped = IndexFileNames::strip_extension(filename);
    let mut parts = stripped[1..].split('_');
    parts.next();
    let generation = parts.next();
    let remaining = parts.count();
    // 4 cases:
    // segment.ext
    // segment_gen.ext
    // segment_codec_suffix.ext
    // segment_gen_codec_suffix.ext
    if (remaining == 0 || remaining == 2)
      && let Some(generation) = generation
    {
      // base-36
      Ok(i64::from_str_radix(generation, 36)?)
    } else {
      Ok(0)
    }
  }
  /// Parses the segment name out of the given file name.
  ///
  /// # Returns
  /// The segment name only, or the filename if it does not contain a `.` and
  /// `_`.
  pub fn parse_segment_name(filename: &str) -> &str {
    if let Some(idx) = IndexFileNames::index_of_segment_name(filename) {
      &filename[..idx]
    } else {
      filename
    }
  }
  /// Removes the extension (anything after the first '.'), otherwise returns
  /// the original filename.
  pub fn strip_extension(filename: &str) -> &str {
    if let Some(idx) = filename.find('.') {
      &filename[..idx]
    } else {
      filename
    }
  }

  /// Return the extension (anything after the first '.'), or None if there is
  /// no '.' in the file name.
  pub fn get_extension(filename: &str) -> Option<&str> {
    if let Some(idx) = filename.find('.') {
      Some(&filename[idx + 1..])
    } else {
      None
    }
  }
}

use regex::Regex;
use std::sync::LazyLock;

pub static CODEC_FILE_PATTERN: LazyLock<Regex> = LazyLock::new(|| {
  expect_invariant!(
    Regex::new(r"_[a-z0-9]+(_.*)?\..*"),
    "codec file pattern is a fixed valid regular expression",
  )
});
