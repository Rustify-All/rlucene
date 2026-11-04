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
use std::borrow::Cow;
use std::fmt::{Display, Formatter};
use std::num::ParseIntError;

use regex::Regex;
use std::sync::LazyLock;
use thiserror::Error;

use crate::core::util::error::lucene_error::LuceneError;
use crate::core::util::error::parse::Parse;
use crate::core::util::error::{IllegalArgumentError, IllegalStateError};
use crate::core::util::strict_string_tokenizer::StrictStringTokenizer;
pub static LUCENE_9_0_0: LazyLock<Version> =
  LazyLock::new(|| expect_invariant!(Version::new(9, 0, 0), "Lucene 9.0.0 is a valid version"));

pub static LUCENE_10_0_0: LazyLock<Version> =
  LazyLock::new(|| expect_invariant!(Version::new(10, 0, 0), "Lucene 10.0.0 is a valid version"));

/// Match settings and bugs in Lucene's 10.1.0 release.
pub static LUCENE_10_1_0: LazyLock<Version> =
  LazyLock::new(|| expect_invariant!(Version::new(10, 1, 0), "Lucene 10.1.0 is a valid version"));

/// Match settings and bugs in Lucene's 11.0.0 release.
pub static LUCENE_10_1_1: LazyLock<Version> =
  LazyLock::new(|| expect_invariant!(Version::new(10, 1, 1), "Lucene 10.1.1 is a valid version"));

/// # Warning
/// If you use this setting, and then upgrade to a newer release of Lucene,
/// sizable changes may happen. If backwards compatibility is important, you
/// should instead explicitly specify an actual version.
///
/// If you use this constant, you may need to **re-index all of your documents**
/// when upgrading Lucene, as the way text is indexed may have changed.
/// Additionally, you may need to **re-test your entire application** to ensure
/// it behaves as expected, as some defaults may have changed and may break
/// functionality in your application.
pub static LATEST: LazyLock<Version> = LazyLock::new(|| LUCENE_10_1_1.clone());
pub static LUCENE_CURRENT: LazyLock<Version> = LazyLock::new(|| LATEST.clone());
pub static MIN_SUPPORTED_MAJOR: LazyLock<i32> = LazyLock::new(|| LATEST.major - 1);
/// Used by certain types to match version compatibility across releases of
/// Lucene.
///
/// # Warning
/// When changing the version parameter that you supply to components in Lucene,
/// do not simply change the version at search-time, but instead also adjust
/// your indexing code to match, and re-index.
#[derive(Clone, PartialEq, Eq, Ord, PartialOrd, Debug, Hash)]
pub struct Version {
  /// Major version, the difference between stable and trunk.
  pub major: i32,
  /// Minor version, incremented within the stable branch.
  pub minor: i32,
  /// Bugfix number, incremented on release branches.
  pub bug_fix: i32,
  /// Prerelease version, currently 0 (alpha), 1 (beta), or 2 (final).
  pub(crate) prerelease: i32,
  encoded_value: i32,
}
impl Version {
  fn new(major: i32, minor: i32, bug_fix: i32) -> Result<Version, IllegalArgumentError> {
    Version::with_prerelease(major, minor, bug_fix, 0)
  }
  fn with_prerelease(
    major: i32,
    minor: i32,
    bug_fix: i32,
    prerelease: i32,
  ) -> Result<Version, IllegalArgumentError> {
    // NOTE: do not enforce major version so we remain future proof, except
    // to make sure it fits in the 8 bits we encode it into:
    if !(0..=255).contains(&major) {
      return Err(IllegalArgumentError::new(format!(
        "Illegal major version: {major}"
      )));
    }
    if !(0..=255).contains(&minor) {
      return Err(IllegalArgumentError::new(format!(
        "Illegal minor version: {minor}"
      )));
    }
    if !(0..=255).contains(&bug_fix) {
      return Err(IllegalArgumentError::new(format!(
        "Illegal bug fix version: {bug_fix}"
      )));
    }
    if !(0..=2).contains(&prerelease) {
      return Err(IllegalArgumentError::new(format!(
        "Illegal pre-release version: {prerelease}"
      )));
    }
    if prerelease != 0 && (minor != 0 || bug_fix != 0) {
      return Err(IllegalArgumentError::new(format!(
        "Prerelease version only supported with major release (got prerelease: {prerelease}, minor: {minor}, bug_fix: {bug_fix})"
      )));
    }
    let encoded_value = (major << 18) | (minor << 10) | (bug_fix << 2) | prerelease;
    debug_assert!(Self::encoded_is_valid(
      major,
      minor,
      bug_fix,
      prerelease,
      encoded_value
    ));
    Ok(Version {
      major,
      minor,
      bug_fix,
      prerelease,
      encoded_value,
    })
  }
  /// Returns true if this version is the same or after the version from the
  /// argument.
  pub fn on_or_after(&self, other: &Version) -> bool {
    self.encoded_value >= other.encoded_value
  }
  /// Parses a version number of the form `"major.minor.bugfix.prerelease"`.
  ///
  /// The `.bugfix` and `.prerelease` parts are optional. Note that this is
  /// forwards compatible: the parsed version does not have to exist as a
  /// constant.
  ///
  /// # Note
  /// This is an internal API.
  pub fn parse(version: &str) -> Result<Version, VersionError> {
    let mut tokens = StrictStringTokenizer::new(version, '.');
    if !tokens.has_more_tokens() {
      return Err(VersionError::parse_error_with_pos(
        format!("Version is not in form major.minor.bugfix(.prerelease) (got: {version})"),
        0,
      ));
    }
    let mut token = tokens.next_token()?;
    let major = token.parse::<i32>().map_err(|e| {
      VersionError::parse_int_error(
        format!("Failed to parse major version from {token} (got: {version})"),
        e,
      )
    })?;

    token = tokens.next_token()?;
    let minor = token.parse::<i32>().map_err(|e| {
      VersionError::parse_int_error(
        format!("Failed to parse minor version from {token} (got: {version})"),
        e,
      )
    })?;

    let mut bug_fix_value: i32 = 0;
    if tokens.has_more_tokens() {
      token = tokens.next_token()?;
      bug_fix_value = match token.parse::<i32>() {
        Ok(v) => v,
        Err(e) => {
          return Err(VersionError::parse_int_error(
            format!("Failed to parse bug fix version from {token} (got: {version})"),
            e,
          ));
        },
      };
    }
    let mut prerelease_value: i32 = 0;
    if tokens.has_more_tokens() {
      token = tokens.next_token()?;
      prerelease_value = token.parse::<i32>().map_err(|e| {
        VersionError::parse_int_error(
          format!("Failed to parse pre-release version from {token} (got: {version})"),
          e,
        )
      })?;
      if prerelease_value == 0 {
        return Err(VersionError::parse_error_with_pos(
          format!(
            "Invalid value {prerelease_value}  for prerelease; should be 1 or 2 (got: {version})"
          ),
          0,
        ));
      }
      if tokens.has_more_tokens() {
        // too many tokens!
        return Err(VersionError::parse_error_with_pos(
          format!("Version is not in form major.minor.bugfix(.prerelease) (got: {version})"),
          0,
        ));
      }
    }
    let result = Version::with_prerelease(major, minor, bug_fix_value, prerelease_value);
    if let Err(e) = result {
      return Err(VersionError::parse_error_with_error(
        format!("failed to parse version string {version}"),
        e.into(),
      ));
    }
    debug_assert!(result.is_ok());
    Ok(result?)
  }
  /// Parses the given version number as a constant or dot-based version.
  ///
  /// This method allows using `"LUCENE_X_Y"` constant names, or version
  /// numbers in the format `"x.y.z"`.
  ///
  /// # Note
  /// This is an internal API.
  pub fn parse_leniently(version: &str) -> Result<Version, VersionError> {
    let version_orig = version;
    let version_upper = version.to_uppercase();

    match version_upper.as_str() {
      "LATEST" | "LUCENE_CURRENT" => Ok(LATEST.clone()),
      _ => {
        let mut version = version_upper;
        let patterns = [
          (r"^LUCENE_(\d+)_(\d+)_(\d+)$", "$1.$2.$3"),
          (r"^LUCENE_(\d+)_(\d+)$", "$1.$2.0"),
          (r"^LUCENE_(\d)(\d)$", "$1.$2.0"),
        ];

        if version.starts_with("LUCENE_") {
          for (pattern, replacement) in patterns.iter() {
            let re = match Regex::new(pattern) {
              Ok(r) => r,
              Err(e) => {
                let err = IllegalArgumentError::new(e.to_string());
                return Err(VersionError::parse_error_with_error("", err.into()));
              },
            };
            if let Cow::Owned(replaced) = re.replace_all(&version, *replacement) {
              version = replaced;
            }
          }
        }

        // Try parsing the modified version string
        match Self::parse(&version) {
          Ok(v) => Ok(v),
          Err(e) => Err(VersionError::parse_error_with_error(
            format!("failed to parse lenient version string {version_orig}"),
            e.into(),
          )),
        }
      },
    }
  }
  /// Returns a new version based on raw numbers.
  ///
  /// # Note
  /// This is an internal API.
  pub fn from_bits(major: i32, minor: i32, bug_fix: i32) -> Result<Version, IllegalArgumentError> {
    Version::new(major, minor, bug_fix)
  }

  fn encoded_is_valid(
    major: i32,
    minor: i32,
    bug_fix: i32,
    prerelease: i32,
    encoded_value: i32,
  ) -> bool {
    debug_assert_eq!(major, ((encoded_value as u32 >> 18) & 0xFF) as i32);
    debug_assert_eq!(minor, ((encoded_value as u32 >> 10) & 0xFF) as i32);
    debug_assert_eq!(bug_fix, ((encoded_value as u32 >> 2) & 0xFF) as i32);
    debug_assert_eq!(prerelease, encoded_value & 0x03);
    true
  }
}
impl Display for Version {
  fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
    if self.prerelease == 0 {
      write!(f, "{}.{}.{}", self.major, self.minor, self.bug_fix)
    } else {
      write!(
        f,
        "{}.{}.{}.{}",
        self.major, self.minor, self.bug_fix, self.prerelease
      )
    }
  }
}

#[derive(Debug, Error, Clone)]
pub enum VersionError {
  #[error("{0}")]
  IllegalState(#[from] IllegalStateError),

  #[error("{0}")]
  IllegalArgument(#[from] IllegalArgumentError),

  #[error("{0}")]
  Parse(#[from] Parse),

  #[error("{message}: {source}")]
  ParseIntError {
    message: String,
    #[source]
    source: ParseIntError,
  },
}

impl VersionError {
  pub fn parse_error_with_pos<T>(msg: T, position: i32) -> Self
  where
    T: Into<String>,
  {
    VersionError::Parse(Parse::new(msg, position))
  }
  pub fn parse_error_with_error<T>(msg: T, error: LuceneError) -> Self
  where
    T: Into<String>,
  {
    VersionError::Parse(Parse::with_error(msg, Option::from(error)))
  }
  pub fn parse_int_error<T>(input: T, source: ParseIntError) -> Self
  where
    T: Into<String>,
  {
    VersionError::ParseIntError {
      message: input.into(),
      source,
    }
  }
}
