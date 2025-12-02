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

use crate::core::index::BytesRef;
use crate::core::store::buffered_checksum_index_input::BufferedChecksumIndexInput;
use crate::core::store::check_sum_index_input::ChecksumIndexInput;
use crate::core::store::data_output::DataOutput;
use crate::core::store::index_input::IndexInput;
use crate::core::store::{DataInput, IndexOutput};
use crate::core::util::StringHelper;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::version::MIN_SUPPORTED_MAJOR;

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
    /// - `Version`: The version number.
    ///
    /// # Errors
    /// - `IoError`: If there is an I/O error writing to the underlying medium.
    /// - `IllegalArgumentError`: If the codec name is not simple ASCII or is
    ///   more than 127 characters in length.
    ///
    /// # See Also
    /// - [`check_header`](CodecUtil::check_header)
    /// - [`header_length`](CodecUtil::header_length)
    pub fn write_header(out: &mut impl DataOutput, codec: &str, version: i32) -> Result<()> {
        let bytes: BytesRef<Vec<u8>> = BytesRef::from_string(codec);
        if bytes.length != codec.len() || bytes.length >= 128 {
            return Err(LuceneError::illegal_argument(format!(
                "codec must be simple ASCII, less than 128 characters in length got {codec}"
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
    /// - `Version`: The version number.
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
    pub fn write_index_header(
        out: &mut impl DataOutput,
        codec: &str,
        version: i32,
        id: &[u8; StringHelper::ID_LENGTH],
        suffix: &str,
    ) -> Result<()> {
        Self::write_header(out, codec, version)?;
        out.write_bytes_range(id, 0, StringHelper::ID_LENGTH as i32)?;
        let suffix_bytes: BytesRef<Vec<u8>> = BytesRef::from_string(suffix);
        if !suffix.is_ascii() || suffix_bytes.length >= 256 {
            return Err(LuceneError::illegal_argument(format!(
                "suffix must be simple ASCII, less than 256 characters in length got {suffix}"
            )));
        }
        out.write_byte(suffix_bytes.length as u8)?;
        out.write_bytes_range(
            &suffix_bytes.bytes,
            suffix_bytes.offset as i32,
            suffix_bytes.length as i32,
        )?;
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
    pub fn header_length(codec: &str) -> i32 {
        9 + codec.len() as i32
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
    pub fn index_header_length(codec: &str, suffix: &str) -> i32 {
        debug_assert!(suffix.len() <= i32::MAX as usize);
        Self::header_length(codec) + StringHelper::ID_LENGTH as i32 + 1 + (suffix.len() as i32)
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
    /// - `CorruptIndexError`: If the first four bytes are not
    ///   [`CodecUtil::CODEC_MAGIC`] or if the codec does not match `codec`.
    /// - `IndexFormatTooOldError`: If the actual version is less than
    ///   `min_version`.
    /// - `IndexFormatTooNewError`: If the actual version is greater than
    ///   `max_version`.
    /// - `IoError`: If there is an I/O error reading from the underlying
    ///   medium.
    ///
    /// # See Also
    /// - [`write_header`](CodecUtil::write_header)
    pub fn check_header(
        data_input: &mut impl DataInput,
        codec: &str,
        min_version: i32,
        max_version: i32,
    ) -> Result<i32> {
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
    pub fn check_header_no_magic(
        data_input: &mut impl DataInput,
        codec: &str,
        min_version: i32,
        max_version: i32,
    ) -> Result<i32> {
        let actual_codec = data_input.read_string()?;
        if actual_codec != codec {
            return Err(LuceneError::corrupt_index(format!(
                "codec mismatch: actual codec={actual_codec} vs expected codec={codec}"
            )));
        }
        let actual_version = Self::read_be_int(data_input)?;
        if actual_version < min_version {
            return Err(LuceneError::index_format_too_old(format!(
                "Format version is not supported (resource {}): {} (needs to be between {} and {}). This version of Lucene only supports indexes created with release {}.0 and later",
                data_input, actual_version, min_version, max_version, *MIN_SUPPORTED_MAJOR
            )));
        }
        if actual_version > max_version {
            return Err(LuceneError::index_format_too_new(format!(
                "Format version is not supported (resource {data_input}): {actual_version} (needs to be between {min_version} and {max_version}) "
            )));
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
    /// - `CorruptIndexError`: If the first four bytes are not
    ///   [`CodecUtil::CODEC_MAGIC`], the codec does not match `codec`, or
    ///   `expected_id` or `expected_suffix` do not match.
    /// - `IndexFormatTooOldError`: If the actual version is less than
    ///   `min_version`.
    /// - `IndexFormatTooNewError`: If the actual version is greater than
    ///   `max_version`.
    /// - `IoError`: If there is an I/O error reading from the underlying
    ///   medium.
    ///
    /// # See Also
    /// - [`write_index_header`](CodecUtil::write_index_header)
    pub fn check_index_header(
        data_input: &mut impl DataInput,
        codec: &str,
        min_version: i32,
        max_version: i32,
        expected_id: &[u8; StringHelper::ID_LENGTH],
        expected_suffix: &str,
    ) -> Result<i32> {
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
    /// - `CorruptIndexError`: If the first four bytes are not
    ///   [`CodecUtil::CODEC_MAGIC`] or if the `expected_id` does not match.
    /// - `IoError`: If there is an I/O error reading from the underlying
    ///   medium.
    ///
    /// # Internal
    /// This is an internal API and is intended for use within Lucene-like
    /// systems.
    pub fn verify_and_copy_index_header(
        data_in: &mut impl IndexInput,
        data_out: &mut impl DataOutput,
        expected_id: &[u8; StringHelper::ID_LENGTH],
    ) -> Result<()> {
        if data_in.length() < (Self::footer_length() + Self::header_length("")) as i64 {
            return Err(LuceneError::corrupt_index(format!(
                "compound sub-files must have a valid codec header and footer: file is too small ({} bytes): (resource={})",
                data_in.length(),
                data_in
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

        let codec = data_in.read_string()?;
        let version = Self::read_be_int(data_in)?;
        Self::check_index_header_id(data_in, expected_id)?;
        let suffix_length = data_in.read_byte()?;
        let mut suffix_bytes: Vec<u8> = vec![0u8; suffix_length as usize];
        data_in.read_bytes(&mut suffix_bytes, 0, suffix_length as i32)?;
        Self::write_be_int(data_out, CodecUtil::CODEC_MAGIC)?;
        data_out.write_string(&codec)?;
        Self::write_be_int(data_out, version)?;
        data_out.write_bytes_range(expected_id, 0, StringHelper::ID_LENGTH as i32)?;
        data_out.write_byte(suffix_length)?;
        data_out.write_bytes_range(&suffix_bytes, 0, suffix_length as i32)?;
        Ok(())
    }
    /// Retrieves the full index header from the provided [`IndexInput`].
    ///
    /// # Errors
    /// - `CorruptIndexError`: If the file does not appear to be a valid index
    ///   file.
    pub fn read_index_header(data_input: &mut impl IndexInput) -> Result<Vec<u8>> {
        // TODO: 跟Java版本确认下 要不要seek(0)
        // data_input.seek(0)?;
        let actual_header = Self::read_be_int(data_input)?;
        if actual_header != CodecUtil::CODEC_MAGIC {
            return Err(LuceneError::corrupt_index(format!(
                "codec header mismatch: actual header={} vs expected header={}",
                actual_header,
                CodecUtil::CODEC_MAGIC
            )));
        }
        let codec = data_input.read_string()?;
        Self::read_be_int(data_input)?;
        data_input.seek(data_input.get_file_pointer() + StringHelper::ID_LENGTH as i64)?;
        let suffix_length = data_input.read_byte()?;
        let bytes_len =
            Self::header_length(&codec) + StringHelper::ID_LENGTH as i32 + 1 + suffix_length as i32;

        let mut bytes: Vec<u8> = vec![0u8; bytes_len as usize];
        data_input.seek(0)?;
        data_input.read_bytes(&mut bytes, 0, bytes_len)?;
        Ok(bytes)
    }

    /// Retrieves the full footer from the provided [`IndexInput`].
    ///
    /// # Errors
    /// - `CorruptIndexError`: If the file does not have a valid footer.
    pub fn read_footer(data_input: &mut impl IndexInput) -> Result<Vec<u8>> {
        if data_input.length() < Self::footer_length() as i64 {
            return Err(LuceneError::corrupt_index(format!(
                "misplaced codec footer (file truncated?): length={} but footerLength=={} (resource={})",
                data_input.length(),
                Self::footer_length(),
                data_input
            )));
        }
        data_input.seek(data_input.length() - Self::footer_length() as i64)?;
        Self::validate_footer(data_input)?;
        data_input.seek(data_input.length() - Self::footer_length() as i64)?;
        let mut bytes: Vec<u8> = vec![0u8; Self::footer_length() as usize];
        data_input.read_bytes(&mut bytes, 0, Self::footer_length())?;
        Ok(bytes)
    }
    /// Expert: reads and verifies the object ID of an index header.
    pub fn check_index_header_id(
        data_input: &mut impl DataInput,
        expected_id: &[u8; StringHelper::ID_LENGTH],
    ) -> Result<()> {
        let mut id = [0u8; StringHelper::ID_LENGTH];
        data_input.read_bytes(&mut id, 0, StringHelper::ID_LENGTH as i32)?;
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
    pub fn check_index_header_suffix(
        data_input: &mut impl DataInput,
        expected_suffix: &str,
    ) -> Result<()> {
        let suffix_length = data_input.read_byte()?;
        let mut suffix: Vec<u8> = vec![0u8; suffix_length as usize];
        data_input.read_bytes(&mut suffix, 0, suffix_length as i32)?;
        let actual_suffix = String::from_utf8(suffix)?;
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
    /// - `IoError`: If there is an I/O error writing to the underlying medium.
    pub fn write_footer(out: &mut impl IndexOutput) -> Result<()> {
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
    pub fn footer_length() -> i32 {
        16
    }

    /// Validates the codec footer previously written by
    /// [`write_footer`](CodecUtil::write_footer).
    ///
    /// # Returns
    /// The actual checksum value.
    ///
    /// # Errors
    /// - `IoError`: If the footer is invalid, the checksum does not match, or
    ///   the input is not properly positioned before the footer at the end of
    ///   the stream.
    pub fn check_footer(checksum_in: &mut impl ChecksumIndexInput) -> Result<i64> {
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
    /// an unexpected exception that has already occurred.
    ///
    /// When a `prior_exception` is provided, this method will add a suppressed
    /// exception indicating whether the checksum for the stream passes,
    /// fails, or cannot be computed, and rethrow it. Otherwise, it behaves
    /// the same as [`check_footer`](CodecUtil::check_footer).
    ///
    /// # Parameters
    /// - `input`: The input stream to validate.
    /// - `prior_exception`: An optional previously occurred exception to
    ///   handle.
    ///
    /// # Errors
    /// - `IoError`: If the footer is invalid, the checksum does not match, or
    ///   the input is not properly positioned before the footer at the end of
    ///   the stream.
    /// - `PriorException`: If a prior exception is provided and rethrown after
    ///   adding supplemental information.
    // TODO:Implemented a naive error propagation mechanism; we may use this
    // error#[source] to standardize error nesting.
    pub fn check_footer_with_error(
        checksum_in: &mut impl ChecksumIndexInput,
        mut prior_error: LuceneError,
    ) -> LuceneError {
        let result = (|prior_error: &mut LuceneError| -> Result<()> {
            // If we have evidence of corruption, then we return the corruption as
            // the main exception and the prior exception gets suppressed.
            // Otherwise, we return the prior exception with a suppressed
            // exception that notifies the user that checksums matched.
            let remaining = checksum_in.length() - checksum_in.get_file_pointer();
            if remaining < Self::footer_length() as i64 {
                // corruption caused us to read into the checksum footer already: we
                // can't proceed
                return Err(LuceneError::corrupt_index(format!(
                    "checksum status indeterminate: remaining={remaining}, ; please run check index for more details: {checksum_in} "
                )));
            } else {
                // otherwise, skip any unread bytes.
                DataInput::skip_bytes(checksum_in, remaining - Self::footer_length() as i64)?;
                // now check the footer
                let checksum = Self::check_footer(checksum_in)?;
                if !matches!(prior_error, LuceneError::IndexFormatTooOld(_)) {
                    let old = std::mem::replace(prior_error, LuceneError::illegal_state("dummy"));
                    *prior_error = LuceneError::corrupt_index_with_source(
                        format!(
                            "checksum passed ({checksum}). possibly transient resource issue, or a Lucene bug"
                        ),
                        old,
                    );
                }
            }
            Ok(())
        })(&mut prior_error);
        match result {
            Ok(_) => prior_error,
            Err(t) => {
                if matches!(t, LuceneError::CorruptIndex(_)) {
                    LuceneError::corrupt_index_with_source(t.to_string(), prior_error)
                } else {
                    LuceneError::corrupt_index_with_source(
                        format!(
                            "checksum status indeterminate: unexpected exception: {}",
                            checksum_in,
                        ),
                        t,
                    )
                }
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
    /// - `IoError`: If the footer is invalid.
    pub fn retrieve_checksum(input: &mut impl IndexInput) -> Result<i64> {
        if input.length() < Self::footer_length() as i64 {
            return Err(LuceneError::corrupt_index(format!(
                "misplaced codec footer (file truncated?): length={} but footerLength=={} (resource={})",
                input.length(),
                Self::footer_length(),
                input
            )));
        }
        input.seek(input.length() - Self::footer_length() as i64)?;
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
    /// - `IoError`: If the footer is invalid.
    pub(crate) fn retrieve_checksum_with_expected(
        input: &mut impl IndexInput,
        expected_length: i64,
    ) -> Result<i64> {
        if expected_length < Self::footer_length() as i64 {
            return Err(LuceneError::illegal_argument(
                "expectedLength cannot be less than the footer length".to_string(),
            ));
        }
        match input.length().cmp(&expected_length) {
            Ordering::Less => {
                return Err(LuceneError::corrupt_index(format!(
                    "truncated file: length={} but expected_length={} (resource={})",
                    input.length(),
                    expected_length,
                    input
                )));
            },
            Ordering::Greater => {
                return Err(LuceneError::corrupt_index(format!(
                    "file too long: length={} but expected_length={} (resource={})",
                    input.length(),
                    expected_length,
                    input
                )));
            },
            Ordering::Equal => {},
        }
        Self::retrieve_checksum(input)
    }

    fn validate_footer(input: &mut impl IndexInput) -> Result<()> {
        let remaining = input.length() - input.get_file_pointer();
        let expected = Self::footer_length();
        match remaining.cmp(&(expected as i64)) {
            Ordering::Less => {
                return Err(LuceneError::corrupt_index(format!(
                    "misplaced codec footer (file truncated?): remaining={}, expected={}, fp={} (resource={})",
                    remaining,
                    expected,
                    input.get_file_pointer(),
                    input
                )));
            },
            Ordering::Greater => {
                return Err(LuceneError::corrupt_index(format!(
                    "misplaced codec footer (file extended?): remaining={}, expected={}, fp={} (resource={})",
                    remaining,
                    expected,
                    input.get_file_pointer(),
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
    pub fn checksum_entire_file(input: &impl IndexInput) -> Result<i64> {
        let mut clone = input.try_clone()?;
        clone.seek(0)?;
        let mut checksum_in = BufferedChecksumIndexInput::new(clone);
        assert_eq!(checksum_in.get_file_pointer(), 0);
        if checksum_in.length() < Self::footer_length() as i64 {
            return Err(LuceneError::corrupt_index(format!(
                "misplaced codec footer (file truncated?): length={} but footerLength=={} (resource={})",
                checksum_in.length(),
                Self::footer_length(),
                input
            )));
        }
        let checksum_len = checksum_in.length();
        IndexInput::seek(
            &mut checksum_in,
            checksum_len - Self::footer_length() as i64,
        )?;
        Self::check_footer(&mut checksum_in)
    }

    /// Reads the CRC32 value as a 64-bit integer from the input.
    ///
    /// # Errors
    /// - `CorruptIndexError`: If the CRC is formatted incorrectly (wrong bits
    ///   set).
    /// - `IoError`: If an I/O error occurs.
    pub fn read_crc(input: &mut impl IndexInput) -> Result<i64> {
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
    /// - `IllegalStateError`: If the CRC is formatted incorrectly (wrong bits
    ///   set).
    /// - `IoError`: If an I/O error occurs.
    pub fn write_crc(out: &mut impl IndexOutput) -> Result<()> {
        let value = out.get_checksum();
        if value & 0xFFFFFFFF00000000 != 0 {
            return Err(LuceneError::illegal_state(format!(
                "Illegal CRC-32 checksum: {value} +  (resource={out})"
            )));
        }
        debug_assert!(value <= i64::MAX as u64);
        Self::write_be_long(out, value as i64)
    }

    /// Writes an integer value to the header or footer in big-endian order.
    pub fn write_be_int(out: &mut impl DataOutput, i: i32) -> Result<()> {
        let bytes = [
            ((i >> 24) & 0xFF) as u8,
            ((i >> 16) & 0xFF) as u8,
            ((i >> 8) & 0xFF) as u8,
            (i & 0xFF) as u8,
        ];
        out.write_bytes_range(&bytes, 0, 4)?;
        Ok(())
    }
    /// Writes a long value to the header or footer in big-endian order.
    pub fn write_be_long(out: &mut impl DataOutput, i: i64) -> Result<()> {
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
    pub fn read_be_int(out: &mut impl DataInput) -> Result<i32> {
        let byte1 = out.read_byte()? as i32;
        let byte2 = out.read_byte()? as i32;
        let byte3 = out.read_byte()? as i32;
        let byte4 = out.read_byte()? as i32;

        Ok((byte1 << 24) | (byte2 << 16) | (byte3 << 8) | byte4)
    }

    /// Reads a long value from the header or footer in big-endian order.
    pub fn read_be_long(out: &mut impl DataInput) -> Result<i64> {
        let mut buffer = [0u8; 8];
        out.read_bytes(&mut buffer, 0, 8)?;
        Ok(i64::from_be_bytes(buffer))
    }
}

#[cfg(test)]
mod tests {
    use std::error::Error;
    use std::fmt::{Display, Formatter};
    use std::sync::atomic::AtomicI64;

    use crate::core::codecs::CodecUtil;
    use crate::core::store::buffered_checksum_index_input::BufferedChecksumIndexInput;
    use crate::core::store::index_input::IndexInput;
    use crate::core::store::{
        ByteBuffersDataOutput, ByteBuffersIndexInput, ByteBuffersIndexOutput, DataInput,
        DataOutput, IndexOutput,
    };
    use crate::core::util::StringHelper;
    use crate::core::util::error::lucene_error::{LuceneError, Result};

    #[allow(dead_code)] // for quick search
    struct TestCodecUtil;

    #[test]
    fn test_header_length() -> Result<()> {
        let mut out = ByteBuffersDataOutput::new();
        {
            let mut output = ByteBuffersIndexOutput::new(&mut out, "temp", "temp");
            CodecUtil::write_header(&mut output, "FooBar", 5)?;
            output.write_string("this is the data")?;
        }

        let mut input = ByteBuffersIndexInput::new(out.get_data_input_ref(), "temp");
        input.seek(CodecUtil::header_length("FooBar") as i64)?;
        assert_eq!(input.read_string()?, "this is the data");
        Ok(())
    }

    #[test]
    fn test_write_too_long_header() -> Result<()> {
        let too_long: String = "a".repeat(128);

        let mut output = ByteBuffersDataOutput::new();
        let mut output = ByteBuffersIndexOutput::new(&mut output, "temp", "temp");

        let result = CodecUtil::write_header(&mut output, &too_long, 5);
        assert!(matches!(result, Err(LuceneError::IllegalArgument(_))));
        Ok(())
    }

    #[test]
    fn test_write_non_ascii_header() -> Result<()> {
        let non_ascii_header = "\u{1234}".to_string();

        let mut out = ByteBuffersDataOutput::new();
        let mut output = ByteBuffersIndexOutput::new(&mut out, "temp", "temp");

        let result = CodecUtil::write_header(&mut output, &non_ascii_header, 5);
        assert!(result.is_ok());
        Ok(())
    }

    #[test]
    fn test_read_header_wrong_magic() -> Result<()> {
        let mut output = ByteBuffersDataOutput::new();
        {
            let mut index_output = ByteBuffersIndexOutput::new(&mut output, "temp", "temp");
            index_output.write_int(1234)?;
        }

        // 创建输入对象
        let input_data = output.get_data_input_ref();
        let mut input = ByteBuffersIndexInput::new(input_data, "temp");

        let result = CodecUtil::check_header(&mut input, "bogus", 1, 1);
        assert!(matches!(result, Err(LuceneError::CorruptIndex(_))));
        Ok(())
    }

    #[test]
    fn test_checksum_entire_file() -> Result<()> {
        let mut output = ByteBuffersDataOutput::new();
        {
            let mut index_output = ByteBuffersIndexOutput::new(&mut output, "temp", "temp");
            CodecUtil::write_header(&mut index_output, "FooBar", 5)?;
            index_output.write_string("this is the data")?;
            CodecUtil::write_footer(&mut index_output)?;
        }

        let input_data = ByteBuffersIndexInput::new(output.get_data_input_ref(), "temp");
        CodecUtil::checksum_entire_file(&input_data)?;
        Ok(())
    }
    #[test]
    // TODO:This test does not reflect the nested error; it needs to be
    // improved.
    fn test_check_footer_valid() -> Result<()> {
        let mut out = ByteBuffersDataOutput::new();
        {
            let mut output = ByteBuffersIndexOutput::new(&mut out, "temp", "temp");
            CodecUtil::write_header(&mut output, "FooBar", 5)?;
            output.write_string("this is the data")?;
            CodecUtil::write_footer(&mut output)?;
        }

        let mut input = BufferedChecksumIndexInput::new(ByteBuffersIndexInput::new(
            out.get_data_input_ref(),
            "temp",
        ));
        let mine = LuceneError::illegal_argument("fake exception");
        let result = CodecUtil::check_footer_with_error(&mut input, mine);
        assert!(result.to_string().contains("checksum passed"));
        Ok(())
    }

    #[test]
    // TODO:This test does not reflect the nested error; it needs to be
    // improved.
    fn test_check_footer_valid_at_footer() -> Result<()> {
        let mut out = ByteBuffersDataOutput::new();
        {
            let mut output = ByteBuffersIndexOutput::new(&mut out, "temp", "temp");
            CodecUtil::write_header(&mut output, "FooBar", 5)?;
            output.write_string("this is the data")?;
            CodecUtil::write_footer(&mut output)?;
        }

        let mut input = BufferedChecksumIndexInput::new(ByteBuffersIndexInput::new(
            out.get_data_input_ref(),
            "temp",
        ));
        CodecUtil::check_header(&mut input, "FooBar", 5, 5)?;
        let read_data = input.read_string()?;
        assert_eq!(read_data, "this is the data");
        let mine = LuceneError::illegal_argument("fake exception");
        let result = CodecUtil::check_footer_with_error(&mut input, mine);
        let err_message = result.to_string();
        assert!(err_message.contains("fake exception"));
        assert!(err_message.contains("checksum passed"));
        Ok(())
    }
    #[test]
    // TODO: This test does not fully reflect the nested error; it needs to be
    // improved.
    fn test_check_footer_valid_past_footer() -> Result<()> {
        let mut out = ByteBuffersDataOutput::new();
        {
            let mut output = ByteBuffersIndexOutput::new(&mut out, "temp", "temp");
            CodecUtil::write_header(&mut output, "FooBar", 5)?;
            output.write_string("this is the data")?;
            CodecUtil::write_footer(&mut output)?;
        }

        let mut input = BufferedChecksumIndexInput::new(ByteBuffersIndexInput::new(
            out.get_data_input_ref(),
            "temp",
        ));

        CodecUtil::check_header(&mut input, "FooBar", 5, 5)?;
        let read_data = input.read_string()?;
        assert_eq!(read_data, "this is the data");

        // Bogusly read a byte too far
        input.read_byte()?;

        let mine = LuceneError::illegal_argument("fake exception");
        let result = CodecUtil::check_footer_with_error(&mut input, mine);
        let err_message = result.to_string();
        assert!(err_message.contains("checksum status indeterminate"));
        assert!(err_message.contains("fake exception"));

        Ok(())
    }
    #[test]
    // TODO: This test does not fully reflect the nested error; it needs to be
    // improved.
    fn test_check_footer_invalid() -> Result<()> {
        let mut out = ByteBuffersDataOutput::new();
        {
            let mut output = ByteBuffersIndexOutput::new(&mut out, "temp", "temp");
            CodecUtil::write_header(&mut output, "FooBar", 5)?;
            output.write_string("this is the data")?;
            CodecUtil::write_be_int(&mut output, CodecUtil::FOOTER_MAGIC)?;
            CodecUtil::write_be_int(&mut output, 0)?;
            CodecUtil::write_be_long(&mut output, 1234567)?; // write a bogus
            // checksum
        }
        let mut input = BufferedChecksumIndexInput::new(ByteBuffersIndexInput::new(
            out.get_data_input_ref(),
            "temp",
        ));
        CodecUtil::check_header(&mut input, "FooBar", 5, 5)?;
        let read_data = input.read_string()?;
        assert_eq!(read_data, "this is the data");
        let mine = LuceneError::illegal_argument("fake exception");
        let result = CodecUtil::check_footer_with_error(&mut input, mine);
        assert!(result.source().is_some());
        let err_message = result.to_string();
        assert!(err_message.contains("checksum failed"));
        assert!(err_message.contains("fake exception"));
        Ok(())
    }
    #[test]
    fn test_segment_header_length() -> Result<()> {
        let mut out = ByteBuffersDataOutput::new();
        {
            let mut output = ByteBuffersIndexOutput::new(&mut out, "temp", "temp");
            let id = StringHelper::random_id();
            CodecUtil::write_index_header(&mut output, "FooBar", 5, &id, "xyz")?;
            output.write_string("this is the data")?;
        }
        let mut input = ByteBuffersIndexInput::new(out.get_data_input_ref(), "temp");

        input.seek(CodecUtil::index_header_length("FooBar", "xyz") as i64)?;

        let read_data = input.read_string()?;
        assert_eq!(read_data, "this is the data");

        Ok(())
    }
    #[test]
    fn test_write_too_long_suffix() {
        let too_long: String = "a".repeat(256);
        let mut out = ByteBuffersDataOutput::new();
        let mut output = ByteBuffersIndexOutput::new(&mut out, "temp", "temp");

        let result = CodecUtil::write_index_header(
            &mut output,
            "foobar",
            5,
            &StringHelper::random_id(),
            &too_long,
        );
        assert!(matches!(result, Err(LuceneError::IllegalArgument(_))));
    }
    #[test]
    fn test_write_very_long_suffix() -> Result<()> {
        let just_long_enough: String = "a".repeat(255);

        let mut out = ByteBuffersDataOutput::new();
        let id = StringHelper::random_id();
        {
            let mut output = ByteBuffersIndexOutput::new(&mut out, "temp", "temp");
            CodecUtil::write_index_header(&mut output, "foobar", 5, &id, &just_long_enough)?;
        }

        let mut input = ByteBuffersIndexInput::new(out.get_data_input_ref(), "temp");
        CodecUtil::check_index_header(&mut input, "foobar", 5, 5, &id, &just_long_enough)?;

        assert_eq!(input.get_file_pointer(), input.length());
        assert_eq!(
            input.get_file_pointer(),
            CodecUtil::index_header_length("foobar", &just_long_enough) as i64
        );

        Ok(())
    }
    #[test]
    fn test_write_non_ascii_suffix() {
        let mut out = ByteBuffersDataOutput::new();
        let mut output = ByteBuffersIndexOutput::new(&mut out, "temp", "temp");

        let non_ascii_suffix = "\u{1234}";

        let result = CodecUtil::write_index_header(
            &mut output,
            "foobar",
            5,
            &StringHelper::random_id(),
            non_ascii_suffix,
        );
        assert!(matches!(result, Err(LuceneError::IllegalArgument(_))));
    }
    #[test]
    fn test_read_bogus_crc() -> Result<()> {
        let mut out = ByteBuffersDataOutput::new();
        {
            let mut output = ByteBuffersIndexOutput::new(&mut out, "temp", "temp");

            CodecUtil::write_be_long(&mut output, -1_i64)?; // bad
            CodecUtil::write_be_long(&mut output, 1_i64 << 32)?; // bad
            CodecUtil::write_be_long(&mut output, -(1_i64 << 32))?; // bad
            CodecUtil::write_be_long(&mut output, (1_i64 << 32) - 1)?; // ok
        }

        let mut input = BufferedChecksumIndexInput::new(ByteBuffersIndexInput::new(
            out.get_data_input_ref(),
            "temp",
        ));

        for _ in 0..3 {
            let result = CodecUtil::read_crc(&mut input);
            assert!(matches!(result, Err(LuceneError::CorruptIndex(_))));
        }

        let result = CodecUtil::read_crc(&mut input);
        assert!(result.is_ok());

        Ok(())
    }

    #[test]
    fn test_write_bogus_crc() -> Result<()> {
        let mut out = ByteBuffersDataOutput::new();
        let output = ByteBuffersIndexOutput::new(&mut out, "temp", "temp");
        let fake_checksum = AtomicI64::new(0);
        let mut fake_output = FakeOutput::new(output, &fake_checksum);

        fake_checksum.store(-1, std::sync::atomic::Ordering::Relaxed); // bad
        let result = CodecUtil::write_crc(&mut fake_output);
        assert!(result.is_err());
        assert!(matches!(result, Err(LuceneError::IllegalState(_))));

        fake_checksum.store(1 << 32, std::sync::atomic::Ordering::Relaxed); // bad
        let result = CodecUtil::write_crc(&mut fake_output);
        assert!(result.is_err());
        assert!(matches!(result, Err(LuceneError::IllegalState(_))));

        fake_checksum.store(-(1 << 32), std::sync::atomic::Ordering::Relaxed); // bad
        let result = CodecUtil::write_crc(&mut fake_output);
        assert!(result.is_err());
        assert!(matches!(result, Err(LuceneError::IllegalState(_))));

        fake_checksum.store((1 << 32) - 1, std::sync::atomic::Ordering::Relaxed); // ok
        let result = CodecUtil::write_crc(&mut fake_output);
        assert!(result.is_ok());

        Ok(())
    }
    #[test]
    // TODO: This test does not fully reflect the nested error; it needs to be
    // improved.
    fn test_truncated_file_throws_corrupt_index_exception() -> Result<()> {
        let mut out = ByteBuffersDataOutput::new();
        let _output = ByteBuffersIndexOutput::new(&mut out, "temp", "temp");

        let mut input = ByteBuffersIndexInput::new(out.get_data_input_ref(), "temp");

        let result = CodecUtil::checksum_entire_file(&input);
        assert!(matches!(result, Err(LuceneError::CorruptIndex(_))));
        assert!(result.unwrap_err().to_string().contains(
            "misplaced codec footer (file truncated?): length=0 but footerLength==16 (resource"
        ));

        let result = CodecUtil::retrieve_checksum(&mut input);
        assert!(matches!(result, Err(LuceneError::CorruptIndex(_))));
        assert!(result.unwrap_err().to_string().contains(
            "misplaced codec footer (file truncated?): length=0 but footerLength==16 (resource"
        ));

        Ok(())
    }

    #[test]
    fn test_retrieve_checksum() {
        // TODO: newDirectory not Implement
    }

    struct FakeOutput<'a> {
        output: ByteBuffersIndexOutput<'a>,
        fake_checksum: &'a AtomicI64,
    }
    impl<'a> FakeOutput<'a> {
        fn new(output: ByteBuffersIndexOutput<'a>, fake_checksum: &'a AtomicI64) -> Self {
            FakeOutput {
                output,
                fake_checksum,
            }
        }
    }

    impl DataOutput for FakeOutput<'_> {
        fn write_byte(&mut self, b: u8) -> Result<()> {
            self.output.write_byte(b)
        }

        fn write_bytes_range(&mut self, b: &[u8], offset: i32, length: i32) -> Result<()> {
            self.output.write_bytes_range(b, offset, length)
        }
    }

    impl Display for FakeOutput<'_> {
        fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
            write!(f, "FakeOutput({})", self.output)
        }
    }

    impl IndexOutput for FakeOutput<'_> {
        fn get_file_pointer(&self) -> i64 {
            self.output.get_file_pointer()
        }

        fn get_checksum(&mut self) -> u64 {
            self.fake_checksum
                .load(std::sync::atomic::Ordering::Relaxed) as u64
        }

        fn get_name(&self) -> &str {
            unreachable!()
        }
    }
}
