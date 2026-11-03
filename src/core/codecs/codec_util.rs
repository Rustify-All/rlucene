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
use std::cmp::Ordering;

use crate::core::store::buffered_checksum_index_input::BufferedChecksumIndexInput;
use crate::core::store::check_sum_index_input::ChecksumIndexInput;
use crate::core::store::data_output::DataOutput;
use crate::core::store::index_input::IndexInput;
use crate::core::store::{DataInput, IndexOutput};
use crate::core::util::error::lucene_error::{
  CaughtResult, CaughtResultExt, LuceneError, LuceneErrorKind, Result,
};
use crate::core::util::{IOUtils, StringHelper, TryIntoInt};

/// Utility struct for reading and writing versioned headers.
///
/// Writing codec headers is useful to ensure that a file is in the format you
/// expect it to be.
///
/// # Experimental
/// This is an experimental API and may be subject to change in future versions.
pub struct CodecUtil;
impl CodecUtil {
  /// Constant to identify the start of a codec header.
  pub const CODEC_MAGIC: i32 = 0x3fd76c17;
  /// Constant to identify the start of a codec footer.
  pub const FOOTER_MAGIC: i32 = !Self::CODEC_MAGIC;
  /// Writes a codec header, which records both a string to identify the file
  /// and a version number. This header can be parsed and validated with
  /// [`check_header`](CodecUtil::check_header).
  ///
  /// # Format
  /// `CodecHeader -> Magic, CodecName, Version`
  ///
  /// - **Magic**:   A `i32` (written using `write_int`). This identifies the
  ///   start of the header.   It is always [`CodecUtil::CODEC_MAGIC`].
  ///
  /// - **CodecName**:   A string (written using `write_string`). This is a
  ///   string to identify this file.
  ///
  /// - **Version**:   A `i32` (written using `write_int`). Records the
  ///   version of the file.
  ///
  /// # Note
  /// The length of a codec header depends only on the name of the codec. This
  /// length can be computed at any time with
  /// [`header_length`](CodecUtil::header_length).
  ///
  /// # Parameters
  /// - `out`: The output stream to write to.
  /// - `codec`: A string to identify this file. It should be simple ASCII and
  ///   less than 128 characters in length.
  /// - `version`: The version number.
  ///
  /// # Errors
  /// - [`LuceneErrorKind::Io`]: If there is an I/O error writing to the underlying medium.
  /// - [`IllegalArgumentError`](crate::core::util::error::IllegalArgumentError): If the codec name is not simple ASCII or is
  ///   more than 127 characters in length.
  ///
  /// # See Also
  /// - [`check_header`](CodecUtil::check_header)
  /// - [`header_length`](CodecUtil::header_length)
  pub fn write_header<DO>(out: &mut DO, codec: &str, version: i32) -> Result<()>
  where
    DO: DataOutput,
  {
    if !codec.is_ascii() || codec.len() >= 128 {
      return Err(LuceneError::illegal_argument(format!(
        "codec must be simple ASCII, less than 128 characters in length [got {codec}]"
      )));
    }
    Self::write_be_int(out, CodecUtil::CODEC_MAGIC)?;
    out.write_string(codec)?;
    Self::write_be_int(out, version)?;
    Ok(())
  }

  /// Writes a codec header, which records both a string to identify the file
  /// and a version number. This header can be parsed and validated with
  /// [`check_header`](CodecUtil::check_header).
  ///
  /// # Format
  /// `CodecHeader -> Magic, CodecName, Version`
  ///
  /// - **Magic**:   A `i32` (written using `write_int`). This identifies the
  ///   start of the header. It is always [`CodecUtil::CODEC_MAGIC`].
  ///
  /// - **CodecName**:   A string (written using `write_string`). This is a
  ///   string to identify this file.
  ///
  /// - **Version**:   A `i32` (written using `write_int`). Records the
  ///   version of the file.
  ///
  /// # Note
  /// The length of a codec header depends only on the name of the codec. This
  /// length can be computed at any time with
  /// [`header_length`](CodecUtil::header_length).
  ///
  /// # Parameters
  /// - `out`: The output stream.
  /// - `codec`: A string to identify this file. It should be simple ASCII and
  ///   less than 128 characters in length.
  /// - `version`: The version number.
  ///
  /// # Errors
  /// - Returns an error if there is an I/O error writing to the underlying
  ///   medium.
  /// - Returns an error if the codec name is not simple ASCII or exceeds 127
  ///   characters in length.
  ///
  /// # See Also
  /// - [`check_header`](CodecUtil::check_header)
  /// - [`header_length`](CodecUtil::header_length)
  pub fn write_index_header<DO>(
    out: &mut DO,
    codec: &str,
    version: i32,
    id: &[u8; StringHelper::ID_LENGTH],
    suffix: &str,
  ) -> Result<()>
  where
    DO: DataOutput,
  {
    Self::write_header(out, codec, version)?;
    out.write_bytes_range(id, 0, StringHelper::ID_LENGTH)?;
    let suffix_bytes = suffix.as_bytes();
    if !suffix.is_ascii() || suffix_bytes.len() >= 256 {
      return Err(LuceneError::illegal_argument(format!(
        "suffix must be simple ASCII, less than 256 characters in length [got {suffix}]"
      )));
    }
    out.write_byte(suffix_bytes.len() as u8)?;
    out.write_bytes_range(suffix_bytes, 0, suffix_bytes.len())?;
    Ok(())
  }
  /// Computes the length of a codec header.
  ///
  /// # Parameters
  /// - `codec`: The codec name.
  ///
  /// # Returns
  /// The length of the entire codec header.
  ///
  /// # See Also
  /// - [`write_header`](CodecUtil::write_header)
  pub fn header_length(codec: &str) -> usize {
    9 + codec.len()
  }
  /// Computes the length of an index header.
  ///
  /// # Parameters
  /// - `codec`: The codec name.
  ///
  /// # Returns
  /// The length of the entire index header.
  ///
  /// # See Also
  /// - [`write_index_header`](CodecUtil::write_index_header)
  pub fn index_header_length(codec: &str, suffix: &str) -> usize {
    Self::header_length(codec) + StringHelper::ID_LENGTH + 1 + suffix.len()
  }
  /// Reads and validates a header previously written with
  /// [`write_header`](CodecUtil::write_header).
  ///
  /// When reading a file, supply the expected `codec` and an expected version
  /// range (`min_version` to `max_version`).
  ///
  /// # Parameters
  /// - `input`: The input stream, positioned at the point where the header
  ///   was previously written. Typically, this is located at the beginning of
  ///   the file.
  /// - `codec`: The expected codec name.
  /// - `min_version`: The minimum supported version number.
  /// - `Max_version`: The maximum supported version number.
  ///
  /// # Returns
  /// The actual version found if a valid header is found that matches
  /// `codec`, with an actual version satisfying `min_version <= actual <=
  /// max_version`. Otherwise, an error is returned.
  ///
  /// # Errors
  /// - [`CorruptIndexError`](crate::core::util::error::CorruptIndexError): If the first four bytes are not
  ///   [`CodecUtil::CODEC_MAGIC`] or if the codec does not match `codec`.
  /// - [`IndexFormatTooOldError`](crate::core::util::error::IndexFormatTooOldError): If the actual
  ///   version is less than
  ///   `min_version`.
  /// - [`IndexFormatTooNewError`](crate::core::util::error::IndexFormatTooNewError): If the actual
  ///   version is greater than
  ///   `max_version`.
  /// - [`LuceneErrorKind::Io`]: If there is an I/O error reading from the underlying
  ///   medium.
  ///
  /// # See Also
  /// - [`write_header`](CodecUtil::write_header)
  pub fn check_header<DI>(
    data_input: &mut DI,
    codec: &str,
    min_version: i32,
    max_version: i32,
  ) -> Result<i32>
  where
    DI: DataInput,
  {
    let actual_header = Self::read_be_int(data_input)?;
    if actual_header != CodecUtil::CODEC_MAGIC {
      return Err(LuceneError::corrupt_index(format!(
        "codec header mismatch: actual header={} vs expected header={}",
        actual_header,
        CodecUtil::CODEC_MAGIC
      )));
    }
    Self::check_header_no_magic(data_input, codec, min_version, max_version)
  }
  /// Similar to [`check_header`](CodecUtil::check_header), except this
  /// version assumes the first `i32` has already been read and validated
  /// from the input.
  ///
  /// # See Also
  /// - [`check_header`](CodecUtil::check_header)
  pub fn check_header_no_magic<DI>(
    data_input: &mut DI,
    codec: &str,
    min_version: i32,
    max_version: i32,
  ) -> Result<i32>
  where
    DI: DataInput,
  {
    let actual_codec = data_input.read_string()?;
    if actual_codec != codec {
      return Err(LuceneError::corrupt_index(format!(
        "codec mismatch: actual codec={actual_codec} vs expected codec={codec}"
      )));
    }
    drop(actual_codec);
    let actual_version = Self::read_be_int(data_input)?;
    if actual_version < min_version {
      return Err(LuceneError::index_format_too_old_with_version(
        data_input,
        actual_version,
        min_version,
        max_version,
      ));
    }
    if actual_version > max_version {
      return Err(LuceneError::index_format_too_new(
        data_input,
        actual_version,
        min_version,
        max_version,
      ));
    }
    Ok(actual_version)
  }
  /// Reads and validates a header previously written with
  /// [`write_index_header`](CodecUtil::write_index_header).
  ///
  /// When reading a file, supply the expected `codec`, expected version range
  /// (`min_version` to `max_version`), object ID, and suffix.
  ///
  /// # Parameters
  /// - `input`: The input stream, positioned at the point where the header
  ///   was previously written. Typically, this is located at the beginning of
  ///   the file.
  /// - `codec`: The expected codec name.
  /// - `min_version`: The minimum supported version number.
  /// - `max_version`: The maximum supported version number.
  /// - `expected_id`: The expected object identifier for this file.
  /// - `Expected_suffix`: The expected auxiliary suffix for this file.
  ///
  /// # Returns
  /// The actual version found, if a valid header is present that matches
  /// `codec`, `expected_id`, and `expected_suffix`, with a version
  /// satisfying `min_version <= actual <= max_version`.
  ///
  /// # Errors
  /// - [`CorruptIndexError`](crate::core::util::error::CorruptIndexError): If the first four bytes are not
  ///   [`CodecUtil::CODEC_MAGIC`], the codec does not match `codec`, or
  ///   `expected_id` or `expected_suffix` do not match.
  /// - [`IndexFormatTooOldError`](crate::core::util::error::IndexFormatTooOldError): If the actual
  ///   version is less than
  ///   `min_version`.
  /// - [`IndexFormatTooNewError`](crate::core::util::error::IndexFormatTooNewError): If the actual
  ///   version is greater than
  ///   `max_version`.
  /// - [`LuceneErrorKind::Io`]: If there is an I/O error reading from the underlying
  ///   medium.
  ///
  /// # See Also
  /// - [`write_index_header`](CodecUtil::write_index_header)
  pub fn check_index_header<DI>(
    data_input: &mut DI,
    codec: &str,
    min_version: i32,
    max_version: i32,
    expected_id: &[u8; StringHelper::ID_LENGTH],
    expected_suffix: &str,
  ) -> Result<i32>
  where
    DI: DataInput,
  {
    let version = Self::check_header(data_input, codec, min_version, max_version)?;
    Self::check_index_header_id(data_input, expected_id)?;
    Self::check_index_header_suffix(data_input, expected_suffix)?;
    Ok(version)
  }

  /// Expert: verifies that the incoming [`IndexInput`] has an index header
  /// and that its segment ID matches the expected one, and then copies
  /// that index header into the provided [`DataOutput`]. This is useful
  /// when building compound files.
  ///
  /// # Parameters
  /// - `input`: The input stream, positioned at the point where the index
  ///   header was previously written. Typically, this is located at the
  ///   beginning of the file.
  /// - `output`: The output stream, where the header will be copied to.
  /// - `expected_id`: The expected segment ID.
  ///
  /// # Errors
  /// - [`CorruptIndexError`](crate::core::util::error::CorruptIndexError): If the first four bytes are not
  ///   [`CodecUtil::CODEC_MAGIC`] or if the `expected_id` does not match.
  /// - [`LuceneErrorKind::Io`]: If there is an I/O error reading from the underlying
  ///   medium.
  ///
  /// # Internal
  /// This is an internal API and is intended for use within Lucene-like
  /// systems.
  pub fn verify_and_copy_index_header<II, DO>(
    data_in: &mut II,
    data_out: &mut DO,
    expected_id: &[u8; StringHelper::ID_LENGTH],
  ) -> Result<()>
  where
    II: IndexInput,
    DO: DataOutput,
  {
    let length = data_in.length()?;
    if length < (Self::footer_length() + Self::header_length("")) {
      return Err(LuceneError::corrupt_index(format!(
        "compound sub-files must have a valid codec header and footer: file is too small ({} bytes): (resource={})",
        length, data_in
      )));
    }
    let actual_header = Self::read_be_int(data_in)?;
    if actual_header != CodecUtil::CODEC_MAGIC {
      return Err(LuceneError::corrupt_index(format!(
        "compound sub-files must have a valid codec header and footer: codec header mismatch: actual header={} vs expected header={}",
        actual_header,
        CodecUtil::CODEC_MAGIC
      )));
    }

    let codec = data_in.read_string()?.into_owned();
    let version = Self::read_be_int(data_in)?;
    Self::check_index_header_id(data_in, expected_id)?;
    let suffix_length = data_in.read_byte()?;
    let suffix_len = suffix_length as usize;
    let suffix_bytes = data_in.get_bytes(suffix_len)?;
    Self::write_be_int(data_out, CodecUtil::CODEC_MAGIC)?;
    data_out.write_string(&codec)?;
    Self::write_be_int(data_out, version)?;
    data_out.write_bytes_range(expected_id, 0, StringHelper::ID_LENGTH)?;
    data_out.write_byte(suffix_length)?;
    data_out.write_bytes_range(&suffix_bytes, 0, suffix_len)?;
    Ok(())
  }
  /// Retrieves the full index header from the provided [`IndexInput`].
  /// The returned bytes may borrow the input; obtain ownership to keep them
  /// across another input operation.
  ///
  /// # Errors
  /// - [`CorruptIndexError`](crate::core::util::error::CorruptIndexError): If the file does not appear to be a valid index
  ///   file.
  pub fn read_index_header<II>(data_input: &mut II) -> Result<Cow<'_, [u8]>>
  where
    II: IndexInput,
  {
    data_input.seek(0)?;
    let actual_header = Self::read_be_int(data_input)?;
    if actual_header != CodecUtil::CODEC_MAGIC {
      return Err(LuceneError::corrupt_index(format!(
        "codec header mismatch: actual header={} vs expected header={}",
        actual_header,
        CodecUtil::CODEC_MAGIC
      )));
    }
    let header_length = {
      let codec = data_input.read_string()?;
      Self::header_length(&codec)
    };
    Self::read_be_int(data_input)?;
    data_input.seek(data_input.get_file_pointer()? + StringHelper::ID_LENGTH)?;
    let suffix_length = data_input.read_byte()? as usize;
    let bytes_len = header_length + StringHelper::ID_LENGTH + 1 + suffix_length;

    data_input.seek(0)?;
    data_input.get_bytes(bytes_len)
  }

  /// Retrieves the full footer from the provided [`IndexInput`].
  /// The returned bytes may borrow the input; obtain ownership to keep them
  /// across another input operation.
  ///
  /// # Errors
  /// - [`CorruptIndexError`](crate::core::util::error::CorruptIndexError): If the file does not have a valid footer.
  pub fn read_footer<II>(data_input: &mut II) -> Result<Cow<'_, [u8]>>
  where
    II: IndexInput,
  {
    let length = data_input.length()?;
    if length < Self::footer_length() {
      return Err(LuceneError::corrupt_index(format!(
        "misplaced codec footer (file truncated?): length={} but footerLength=={} (resource={})",
        length,
        Self::footer_length(),
        data_input
      )));
    }
    let footer_len = Self::footer_length();
    data_input.seek(length - footer_len)?;
    Self::validate_footer(data_input)?;
    data_input.seek(length - footer_len)?;
    data_input.get_bytes(footer_len)
  }
  /// Expert: reads and verifies the object ID of an index header.
  pub fn check_index_header_id<DI>(
    data_input: &mut DI,
    expected_id: &[u8; StringHelper::ID_LENGTH],
  ) -> Result<()>
  where
    DI: DataInput,
  {
    let mut id = [0u8; StringHelper::ID_LENGTH];
    data_input.read_bytes(&mut id, 0, StringHelper::ID_LENGTH)?;
    if id != *expected_id {
      return Err(LuceneError::corrupt_index(format!(
        "file mismatch, expected id={}, got={} (resource={})",
        StringHelper::id_to_string(Option::from(expected_id)),
        StringHelper::id_to_string(Option::from(&id)),
        data_input
      )));
    }
    Ok(())
  }
  /// Expert: reads and verifies the suffix of an index header.
  pub fn check_index_header_suffix<DI>(data_input: &mut DI, expected_suffix: &str) -> Result<()>
  where
    DI: DataInput,
  {
    let suffix_length = data_input.read_byte()? as usize;
    if suffix_length == 0 {
      data_input.read_bytes(&mut [], 0, 0)?;
      if !expected_suffix.is_empty() {
        return Err(LuceneError::corrupt_index(format!(
          "file mismatch, expected suffix={expected_suffix}, got= (resource={data_input})"
        )));
      }
      return Ok(());
    }
    let suffix = data_input.get_bytes(suffix_length)?;
    if suffix.as_ref() == expected_suffix.as_bytes() {
      return Ok(());
    }
    // Keep diagnostic bytes only when comparison needs the input's Display.
    let mut actual_bytes = [0u8; u8::MAX as usize];
    actual_bytes[..suffix_length].copy_from_slice(suffix.as_ref());
    drop(suffix);
    let actual_suffix = String::from_utf8_lossy(&actual_bytes[..suffix_length]);
    if actual_suffix != expected_suffix {
      return Err(LuceneError::corrupt_index(format!(
        "file mismatch, expected suffix={expected_suffix}, got={actual_suffix} (resource={data_input})"
      )));
    }
    Ok(())
  }
  /// Writes a codec footer, which records both a checksum algorithm ID and a
  /// checksum. This footer can be parsed and validated with
  /// [`check_footer`](CodecUtil::check_footer).
  ///
  /// # Format
  /// `CodecFooter -> Magic, AlgorithmID, Checksum`
  ///
  /// - **Magic**:   A `i32` (written using `write_int`). This identifies the
  ///   start of the footer.   It is always [`CodecUtil::FOOTER_MAGIC`].
  ///
  /// - **AlgorithmID**:   A `i32` (written using `write_int`). This indicates
  ///   the checksum algorithm used.   Currently, this is always 0 for
  ///   zlib-crc32.
  ///
  /// - **Checksum**:   A `u64` (written using `write_long`). The actual
  ///   checksum value for all previous bytes in the stream,   including the
  ///   bytes from Magic and AlgorithmID.
  ///
  /// # Parameters
  /// - `out`: The output stream to write to.
  ///
  /// # Errors
  /// - [`LuceneErrorKind::Io`]: If there is an I/O error writing to the underlying medium.
  pub fn write_footer<IO>(out: &mut IO) -> Result<()>
  where
    IO: IndexOutput,
  {
    Self::write_be_int(out, CodecUtil::FOOTER_MAGIC)?;
    Self::write_be_int(out, 0)?;
    Self::write_crc(out)?;
    Ok(())
  }

  /// Computes the length of a codec footer.
  ///
  /// # Returns
  /// The length of the entire codec footer.
  ///
  /// # See Also
  /// - [`write_footer`](CodecUtil::write_footer)
  pub fn footer_length() -> usize {
    16
  }

  /// Validates the codec footer previously written by
  /// [`write_footer`](CodecUtil::write_footer).
  ///
  /// # Returns
  /// The actual checksum value.
  ///
  /// # Errors
  /// - [`LuceneErrorKind::Io`]: If the footer is invalid, the checksum does not match, or
  ///   the input is not properly positioned before the footer at the end of
  ///   the stream.
  pub fn check_footer<II>(checksum_in: &mut II) -> Result<i64>
  where
    II: ChecksumIndexInput,
  {
    Self::validate_footer(checksum_in)?;
    let actual_checksum = checksum_in.get_checksum();
    let expected_checksum = Self::read_crc(checksum_in)?;
    if actual_checksum != expected_checksum {
      return Err(LuceneError::corrupt_index(format!(
        "checksum failed (hardware problem?): expected={expected_checksum} but got={actual_checksum} (resource={checksum_in})"
      )));
    }
    Ok(actual_checksum)
  }

  /// Validates the codec footer previously written by
  /// [`write_footer`](CodecUtil::write_footer), optionally handling
  /// an unexpected error that has already occurred.
  ///
  /// When a failed `prior_result` is provided, this method adds a suppressed
  /// error indicating whether the checksum for the stream passes, fails, or
  /// cannot be computed, then returns the original error or resumes the
  /// original panic. Otherwise, it behaves like
  /// [`check_footer`](CodecUtil::check_footer).
  ///
  /// # Parameters
  /// - `input`: The input stream to validate.
  /// - `prior_result`: An optional caught result containing a previously
  ///   occurred error or panic.
  ///
  /// # Errors
  /// - [`LuceneErrorKind::Io`]: If the footer is invalid, the checksum does not match, or
  ///   the input is not properly positioned before the footer at the end of
  ///   the stream.
  /// - `prior_result`: If a prior failure is provided, it is propagated after
  ///   adding supplemental information unless footer corruption replaces it.
  pub fn check_footer_with_error<T, II>(
    checksum_in: &mut II,
    prior_result: Option<CaughtResult<T>>,
  ) -> Result<()>
  where
    II: ChecksumIndexInput,
  {
    let Some(mut prior_result) = prior_result else {
      return Self::check_footer(checksum_in).map(|_| ());
    };
    if matches!(&prior_result, Ok(Ok(_))) {
      return Err(LuceneError::illegal_argument(
        "prior result must contain an error or panic",
      ));
    }

    let footer_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<()> {
      // If we have evidence of corruption, then we return the corruption as
      // the main error and the prior error gets suppressed.
      // Otherwise, we return the prior error with a suppressed
      // error that notifies the user that checksums matched.
      let remaining = checksum_in
        .length()?
        .checked_sub(checksum_in.get_file_pointer()?)
        .ok_or_else(|| LuceneError::corrupt_index(format!(
          "checksum status indeterminate: file pointer exceeds file length; please run checkindex for more details: {checksum_in}"
        )))?;
      if remaining < Self::footer_length() {
        // corruption caused us to read into the checksum footer already: we
        // can't proceed
        Err(LuceneError::corrupt_index(format!(
          "checksum status indeterminate: remaining={remaining}; please run checkindex for more details: {checksum_in}"
        )))
      } else {
        // otherwise, skip any unread bytes.
        DataInput::skip_bytes(
          checksum_in,
          (remaining - Self::footer_length()).try_convert()?,
        )?;
        // now check the footer
        let checksum = Self::check_footer(checksum_in)?;
        if !matches!(
          prior_result
            .as_ref()
            .map(|result| result.as_ref().map_err(LuceneError::kind)),
          Ok(Err(LuceneErrorKind::IndexFormatTooOld(_)))
        ) {
          // If the index format is too old and no corruption, do not add a
          // checksums-matching message since this may tend to unnecessarily
          // alarm people who see "Lucene bug" in their logs.
          prior_result.add_suppressed::<()>(
              Ok(Err(LuceneError::corrupt_index(format!(
                "checksum passed ({checksum:x}). possibly transient resource issue, or a Lucene bug: {checksum_in}"
              )))),
              "panic while recording a successful footer check",
            );
        }
        Ok(())
      }
    }));
    match footer_result {
      Ok(Ok(())) => IOUtils::rethrow_always(prior_result),
      Ok(Err(mut error)) if matches!(error.kind(), LuceneErrorKind::CorruptIndex(_)) => {
        if let Some(prior_error) =
          prior_result.caught_failure("panic before checking the codec footer")
        {
          error.add_suppressed(prior_error);
        }
        Err(error)
      },
      footer_result => {
        let mut suppressed = LuceneError::corrupt_index(format!(
          "checksum status indeterminate: unexpected error: {checksum_in}"
        ));
        if let Some(footer_error) =
          footer_result.caught_failure("panic while checking the codec footer")
        {
          suppressed.add_suppressed(footer_error);
        }
        prior_result.add_suppressed::<()>(
          Ok(Err(suppressed)),
          "panic while recording an indeterminate footer check",
        );
        IOUtils::rethrow_always(prior_result)
      },
    }
  }

  /// Returns (but does not validate) the checksum previously written by
  /// [`check_footer`](CodecUtil::check_footer).
  ///
  /// # Returns
  /// The actual checksum value.
  ///
  /// # Errors
  /// - [`LuceneErrorKind::Io`]: If the footer is invalid.
  pub fn retrieve_checksum<II>(input: &mut II) -> Result<i64>
  where
    II: IndexInput,
  {
    let length = input.length()?;
    if length < Self::footer_length() {
      return Err(LuceneError::corrupt_index(format!(
        "misplaced codec footer (file truncated?): length={} but footerLength=={} (resource={})",
        length,
        Self::footer_length(),
        input
      )));
    }
    input.seek(length - Self::footer_length())?;
    Self::validate_footer(input)?;
    Self::read_crc(input)
  }

  /// Returns (but does not validate) the checksum previously written by
  /// [`check_footer`](CodecUtil::check_footer).
  ///
  /// # Returns
  /// The actual checksum value.
  ///
  /// # Errors
  /// - [`LuceneErrorKind::Io`]: If the footer is invalid.
  pub(crate) fn retrieve_checksum_with_expected<II>(
    input: &mut II,
    expected_length: usize,
  ) -> Result<i64>
  where
    II: IndexInput,
  {
    if expected_length < Self::footer_length() {
      return Err(LuceneError::illegal_argument(
        "expectedLength cannot be less than the footer length".to_string(),
      ));
    }
    let length = input.length()?;
    match length.cmp(&expected_length) {
      Ordering::Less => {
        return Err(LuceneError::corrupt_index(format!(
          "truncated file: length={} but expected_length={} (resource={})",
          length, expected_length, input
        )));
      },
      Ordering::Greater => {
        return Err(LuceneError::corrupt_index(format!(
          "file too long: length={} but expected_length={} (resource={})",
          length, expected_length, input
        )));
      },
      Ordering::Equal => {},
    }
    Self::retrieve_checksum(input)
  }

  fn validate_footer<II>(input: &mut II) -> Result<()>
  where
    II: IndexInput,
  {
    let remaining = input
      .length()?
      .checked_sub(input.get_file_pointer()?)
      .ok_or_else(|| {
        LuceneError::corrupt_index(format!(
          "misplaced codec footer (file truncated?): file pointer exceeds file length (resource={input})"
        ))
      })?;
    let expected = Self::footer_length();
    match remaining.cmp(&(expected)) {
      Ordering::Less => {
        return Err(LuceneError::corrupt_index(format!(
          "misplaced codec footer (file truncated?): remaining={}, expected={}, fp={} (resource={})",
          remaining,
          expected,
          input.get_file_pointer()?,
          input
        )));
      },
      Ordering::Greater => {
        return Err(LuceneError::corrupt_index(format!(
          "misplaced codec footer (file extended?): remaining={}, expected={}, fp={} (resource={})",
          remaining,
          expected,
          input.get_file_pointer()?,
          input
        )));
      },
      Ordering::Equal => {},
    }
    let magic = Self::read_be_int(input)?;
    if magic != CodecUtil::FOOTER_MAGIC {
      return Err(LuceneError::corrupt_index(format!(
        "codec footer mismatch  (file truncated?): actual footer={} vs expected footer={} (resource={})",
        magic,
        CodecUtil::FOOTER_MAGIC,
        input
      )));
    }
    let algorithm_id = Self::read_be_int(input)?;
    if algorithm_id != 0 {
      return Err(LuceneError::corrupt_index(format!(
        "codec footer mismatch: unknown algorithmID={algorithm_id} (resource={input})"
      )));
    }
    Ok(())
  }

  /// Clones the provided input, reads all bytes from the file, and calls
  /// [`check_footer`](CodecUtil::check_footer).
  ///
  /// # Note
  /// This method may be slow, as it must process the entire file.  
  /// If you just need to extract the checksum value, call
  /// [`retrieve_checksum`](CodecUtil::retrieve_checksum).
  pub fn checksum_entire_file<II>(input: &II) -> Result<i64>
  where
    II: IndexInput,
  {
    let mut clone = input.try_clone()?;
    clone.seek(0)?;
    let mut checksum_in = BufferedChecksumIndexInput::new(clone);
    debug_assert_eq!(checksum_in.get_file_pointer()?, 0);
    let checksum_len = checksum_in.length()?;
    if checksum_len < Self::footer_length() {
      return Err(LuceneError::corrupt_index(format!(
        "misplaced codec footer (file truncated?): length={} but footerLength=={} (resource={})",
        checksum_len,
        Self::footer_length(),
        input
      )));
    }
    let v = checksum_len
      .checked_sub(Self::footer_length())
      .ok_or_else(|| {
        LuceneError::illegal_state(format!(
          "underflow, checksum_len {}, foot_length {} ",
          checksum_len,
          Self::footer_length()
        ))
      })?;
    IndexInput::seek(&mut checksum_in, v)?;
    Self::check_footer(&mut checksum_in)
  }

  /// Reads the CRC32 value as a 64-bit integer from the input.
  ///
  /// # Errors
  /// - [`CorruptIndexError`](crate::core::util::error::CorruptIndexError): If the CRC is formatted incorrectly (wrong bits
  ///   set).
  /// - [`LuceneErrorKind::Io`]: If an I/O error occurs.
  pub fn read_crc<II>(input: &mut II) -> Result<i64>
  where
    II: IndexInput,
  {
    let value = Self::read_be_long(input)?;
    if (value as u64) & 0xFFFFFFFF00000000 != 0 {
      return Err(LuceneError::corrupt_index(format!(
        "Illegal CRC-32 checksum: {value} (resource={input})"
      )));
    }
    Ok(value)
  }

  /// Writes the CRC32 value as a 64-bit integer to the output.
  ///
  /// # Errors
  /// - [`IllegalStateError`](crate::core::util::error::IllegalStateError): If the CRC is formatted incorrectly (wrong bits
  ///   set).
  /// - [`LuceneErrorKind::Io`]: If an I/O error occurs.
  pub fn write_crc<IO>(out: &mut IO) -> Result<()>
  where
    IO: IndexOutput,
  {
    let value = out.get_checksum()?;
    if value & 0xFFFFFFFF00000000 != 0 {
      return Err(LuceneError::illegal_state(format!(
        "Illegal CRC-32 checksum: {value} +  (resource={out})"
      )));
    }
    debug_assert!(value <= i64::MAX as u64);
    Self::write_be_long(out, value as i64)
  }

  /// Writes an integer value to the header or footer in big-endian order.
  pub fn write_be_int<DO>(out: &mut DO, i: i32) -> Result<()>
  where
    DO: DataOutput,
  {
    let bytes = [
      ((i >> 24) & 0xFF) as u8,
      ((i >> 16) & 0xFF) as u8,
      ((i >> 8) & 0xFF) as u8,
      (i & 0xFF) as u8,
    ];
    out.write_bytes_range(&bytes, 0, 4)?;
    Ok(())
  }
  /// Writes an `i64` value to the header or footer in big-endian order.
  pub fn write_be_long<DO>(out: &mut DO, i: i64) -> Result<()>
  where
    DO: DataOutput,
  {
    let bytes = [
      ((i >> 56) & 0xFF) as u8,
      ((i >> 48) & 0xFF) as u8,
      ((i >> 40) & 0xFF) as u8,
      ((i >> 32) & 0xFF) as u8,
      ((i >> 24) & 0xFF) as u8,
      ((i >> 16) & 0xFF) as u8,
      ((i >> 8) & 0xFF) as u8,
      (i & 0xFF) as u8,
    ];
    out.write_bytes_range(&bytes, 0, 8)?;
    Ok(())
  }
  /// Reads an integer value from the header or footer in big-endian order.
  pub fn read_be_int<DI>(out: &mut DI) -> Result<i32>
  where
    DI: DataInput,
  {
    let byte1 = out.read_byte()? as i32;
    let byte2 = out.read_byte()? as i32;
    let byte3 = out.read_byte()? as i32;
    let byte4 = out.read_byte()? as i32;

    Ok((byte1 << 24) | (byte2 << 16) | (byte3 << 8) | byte4)
  }

  /// Reads an `i64` value from the header or footer in big-endian order.
  pub fn read_be_long<DI>(out: &mut DI) -> Result<i64>
  where
    DI: DataInput,
  {
    let mut buffer = [0u8; 8];
    out.read_bytes(&mut buffer, 0, 8)?;
    Ok(i64::from_be_bytes(buffer))
  }
}
