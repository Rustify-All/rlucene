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
use std::collections::{HashMap, HashSet};
use std::fmt;
use std::sync::Arc;

use once_cell::sync::Lazy;
use parking_lot::Mutex;

use crate::core::codecs::lucene101_codec::Lucene101Codec;
use crate::core::codecs::segment_info_format::SegmentInfoFormat;
use crate::core::codecs::{Codec, CodecUtil, LATEST_CODEC, get_default_code};
use crate::core::index::IndexFileNames;
use crate::core::index::index_commit::IndexCommit;

use crate::core::index::index_writer::get_actual_max_docs;
use crate::core::index::segment_commit_info::SegmentCommitInfo;
use crate::core::store::check_sum_index_input::ChecksumIndexInput;
use crate::core::store::directory::Directory;
use crate::core::store::{DataInput, IO_CONTEXT_DEFAULT, IndexOutput};
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::output_enum::OutputEnum;
use crate::core::util::{IOUtils, LATEST, MIN_SUPPORTED_MAJOR, StringHelper, Version};
use num_bigint::BigInt;
use std::io::Write;

static INFO_STREAM: Lazy<Mutex<Option<Arc<Mutex<OutputEnum>>>>> = Lazy::new(|| Mutex::new(None));

/// A collection of `SegmentInfo` objects with methods for operating on those
/// segments in relation to the file system.
///
/// The active segments in the index are stored in the segment info file,
/// `segments_N`. There may be one or more `segments_N` files in the index;
/// however, the one with the largest generation is the active one (when older
/// `segments_N` files are present it's because they temporarily cannot be
/// deleted, or a custom
/// [`IndexDeletionPolicy`](crate::core::index::index_deletion_policy) is in use).
/// This file lists each segment by name and has details about the codec and
/// generation of deletes.
///
/// Files:
///
/// - `segments_N`: Header, LuceneVersion, Version, NameCounter, SegCount,
///   MinSegmentLuceneVersion, `<SegName, SegID, SegCodec, DelGen,
///   DeletionCount, FieldInfosGen, DocValuesGen,
///   UpdatesFiles><sup>SegCount</sup>, CommitUserData, Footer
///
/// Data types:
///
/// - `Header` -> [`IndexHeader`](CodecUtil::write_index_header)
/// - `LuceneVersion` -> Which Lucene code [`Version`] was used for this commit,
///   written as three
///   [`DataOutput::writeVInt`](crate::core::store::data_output::DataOutput::write_vint):
///   major, minor, bugfix
/// - `MinSegmentLuceneVersion` -> Lucene code [`Version`] of the oldest
///   segment, written as three
///   [`DataOutput::writeVInt`](crate::core::store::data_output::DataOutput::write_vint):
///   major, minor, bugfix; this is only written only if there's at least one
///   segment
/// - `NameCounter`, `SegCount`, `DeletionCount` ->
///   [`DataOutput::writeInt`](crate::core::store::data_output::DataOutput::write_int)
/// - `Generation`, `Version`, `DelGen`, `Checksum`, `FieldInfosGen`,
///   `DocValuesGen` ->
///   [`DataOutput::writeLong`](crate::core::store::data_output::DataOutput::write_long)
/// - `SegID` ->
///   [`DataOutput::writeByte`](crate::core::store::data_output::DataOutput::write_byte)
/// - `SegName`, `SegCodec` ->
///   [`DataOutput::writeString`](crate::core::store::data_output::DataOutput::write_string)
/// - `CommitUserData` ->
///   [`DataOutput::writeMapOfStrings`](crate::core::store::data_output::DataOutput::write_map_of_strings)
/// - `UpdatesFiles` ->
///   Map<[`DataOutput::writeInt`](crate::core::store::data_output::DataOutput::write_int),
///   [`DataOutput::writeSetOfStrings`](crate::core::store::data_output::DataOutput::write_set_of_strings)>
/// - `Footer` -> [`CodecUtil::writeFooter`](CodecUtil::write_footer)
///
/// Field Descriptions:
///
/// - `Version` counts how often the index has been changed by adding or
///   deleting documents.
/// - `NameCounter` is used to generate names for new segment files.
/// - `SegName` is the name of the segment, and is used as the file name prefix
///   for all of the files that compose the segment's index.
/// - `DelGen` is the generation count of the delete file. If this is `-1`,
///   there are no deletes. Anything above zero means there are deletes stored
///   by [`LiveDocsFormat`](crate::core::codecs::live_docs_format).
/// - `DeletionCount` records the number of deleted documents in this segment.
/// - `SegCodec` is the [`Codec::getName`](Codec::get_name) of the Codec that
///   encoded this segment.
/// - `SegID` is the identifier of the Codec that encoded this segment.
/// - `CommitUserData` stores an optional user-supplied opaque
///   `Map<String,String>` that was passed to
///   [`IndexWriter::setLiveCommitData`](IndexWriter::set_live_commit_data).
/// - `FieldInfosGen` is the generation count of the fieldInfos file. If this is
///   `-1`, there are no updates to the fieldInfos in that segment. Anything
///   above zero means there are updates to fieldInfos stored by
///   [`FieldInfosFormat`](crate::core::codecs::field_infos_format::FieldInfosFormat).
/// - `DocValuesGen` is the generation count of the updatable DocValues. If this
///   is `-1`, there are no updates to DocValues in that segment. Anything above
///   zero means there are updates to DocValues stored by
///   [`DocValuesFormat`](crate::core::codecs::doc_values_format::DocValuesFormat).
/// - `UpdatesFiles` stores the set of files that were updated in that segment
///   per field.
///
/// # Note
/// This module is experimental and subject to change.
pub struct SegmentInfos<D>
where
    D: Directory,
{
    /// Used to name new segments.
    pub counter: i64,
    /// Counts how often the index has been changed.
    pub version: i64,
    /// Generation of the "segments_N" for the next commit.
    generation: i64,
    /// Generation of the "segments_N" file we last successfully read or wrote.
    last_generation: i64,
    /// Opaque `HashMap<String, String>` that user can specify during
    /// `IndexWriter.commit`.
    pub user_data: HashMap<String, String>,
    /// List of `SegmentCommitInfo` objects.
    pub(crate) segments: HashMap<String, SegmentCommitInfo<D>>,
    // map segment name to SegmentCommitInfo
    pub(crate) segments_idx: Vec<String>,
    /// ID for this commit; only written starting with Lucene 5.0.
    id: Option<[u8; StringHelper::ID_LENGTH]>,
    /// Which Lucene version wrote this commit?
    lucene_version: Option<Version>,
    /// Version of the oldest segment in the index, or `None` if there are no
    /// segments.
    min_segment_lucene_version: Option<Version>,
    /// The Lucene version major that was used to create the index.
    index_created_version_major: i32,
    // Only true after prepareCommit has been called and
    // before finishCommit is called
    pub(crate) pending_commit: bool,
}

impl<D> SegmentInfos<D>
where
    D: Directory,
{
    /// Sole constructor.
    ///
    /// # Arguments
    /// - `index_created_version_major`: The Lucene version major at index
    ///   creation time, or 6 if the index was created before 7.0.
    pub fn new(index_created_version_major: i32) -> Result<SegmentInfos<D>> {
        if index_created_version_major > LATEST.major {
            return Err(LuceneError::illegal_argument(format!(
                "indexCreatedVersionMajor is in the future: {index_created_version_major}"
            )));
        }
        if index_created_version_major < 6 {
            return Err(LuceneError::illegal_argument(format!(
                "indexCreatedVersionMajor must be >= 6, got: {index_created_version_major}"
            )));
        }

        Ok(SegmentInfos {
            counter: 0,
            version: 0,
            generation: 0,
            last_generation: 0,
            user_data: HashMap::new(),
            segments: HashMap::new(),
            id: None,
            lucene_version: None,
            min_segment_lucene_version: None,
            index_created_version_major,
            pending_commit: false,
            segments_idx: Vec::new(),
        })
    }
    /// Returns [`SegmentCommitInfo`] at the provided index.
    pub fn info(&self, i: &str) -> Option<&SegmentCommitInfo<D>> {
        self.segments.get(i)
    }
    pub fn info_idx(&self, i: usize) -> Option<&SegmentCommitInfo<D>> {
        let str = self.segments_idx.get(i);
        match str {
            Some(s) => self.segments.get(s),
            None => None,
        }
    }
    pub fn info_mut(&mut self, i: &str) -> Option<&mut SegmentCommitInfo<D>> {
        self.segments.get_mut(i)
    }

    /// Get the segments_N filename in use by this segment infos.
    pub fn get_segments_file_name(&self) -> Option<String> {
        IndexFileNames::file_name_from_generation(
            IndexFileNames::SEGMENTS,
            "",
            self.last_generation,
        )
    }

    /// Returns the generation of the next pending `segments_N` that will be
    /// written.
    pub fn get_next_pending_generation(&self) -> i64 {
        if self.generation == -1 {
            1
        } else {
            self.generation + 1
        }
    }

    /// Since Lucene 5.0, every commit (`segments_N`) writes a unique id. This
    /// will return that id.
    pub fn get_id(&self) -> Option<&[u8; StringHelper::ID_LENGTH]> {
        self.id.as_ref()
    }
    /// Read a particular `segmentFileName`. This may throw an error if a commit
    /// is in process.
    ///
    /// # Arguments
    ///
    /// - `Directory`: Directory containing the segment file.
    /// - `Segment_file_name`: The segment file to load.
    ///
    /// # Errors
    ///
    /// - Returns `LuceneError::CorruptIndex` if the index is corrupt.
    /// - Returns `LuceneError` for any low-level IO error.
    pub fn read_commit(directory: Arc<D>, segment_file_name: &str) -> Result<SegmentInfos<D>> {
        Self::read_commit_with_file_min_version(directory, segment_file_name, *MIN_SUPPORTED_MAJOR)
    }

    /// Reads a particular `segmentFileName`, as long as the commits
    /// [`SegmentInfos::get_index_created_version_major`](SegmentInfos::get_index_created_version_major)
    /// is strictly greater than the provided minimum supported major version.
    ///
    /// If the commits version is older,
    /// an [`IndexFormatTooOldException`](LuceneError::index_format_too_old)
    /// will be thrown.
    /// Note that this may return an `Err` if a commit is in process.
    pub fn read_commit_with_file_min_version(
        directory: Arc<D>,
        segment_file_name: &str,
        min_supported_major_version: i32,
    ) -> Result<SegmentInfos<D>> {
        let generation = generation_from_segments_file_name(segment_file_name)?;
        let mut input;
        {
            input = match directory.open_checksum_input(segment_file_name) {
                Ok(input) => input,
                Err(e) => {
                    return Err(LuceneError::corrupt_index(format!(
                        "Unexpected file read error while opening index: {e}"
                    )));
                },
            };
        }

        match SegmentInfos::read_commit_impl(
            directory.clone(),
            &mut input,
            generation,
            min_supported_major_version,
        ) {
            Ok(commit) => Ok(commit),
            Err(e) => Err(LuceneError::corrupt_index(format!(
                "Unexpected file read error while reading index: {e:?}"
            ))),
        }
    }

    /// Read the commit from the provided [`ChecksumIndexInput`].
    pub fn read_commit_with_input(
        directory: Arc<D>,
        input: &mut impl ChecksumIndexInput,
        generation: i64,
    ) -> Result<Self> {
        Self::read_commit_impl(directory, input, generation, *MIN_SUPPORTED_MAJOR)
    }
    /// Read the commit from the provided [`ChecksumIndexInput`].
    pub fn read_commit_impl(
        directory: Arc<D>,
        input: &mut impl ChecksumIndexInput,
        generation: i64,
        min_supported_major_version: i32,
    ) -> Result<Self> {
        let mut prior_error: Option<LuceneError> = None;

        // Read the magic number
        let magic = CodecUtil::read_be_int(input)?;
        if magic != CodecUtil::CODEC_MAGIC {
            return Err(LuceneError::index_format_too_old(format!(
                "Format version is not supported (resource {}): {} (needs to be between {} and {}). This version of Lucene only supports indexes created with release {}.0 and later",
                input,
                magic,
                CodecUtil::CODEC_MAGIC,
                CodecUtil::CODEC_MAGIC,
                *MIN_SUPPORTED_MAJOR
            )));
        }
        let format =
            CodecUtil::check_header_no_magic(input, "segments", VERSION_74, VERSION_CURRENT)?;

        // Read the ID
        let mut id = [0u8; StringHelper::ID_LENGTH];
        let id_len = id.len();
        debug_assert!(id_len <= i32::MAX as usize);
        input.read_bytes(&mut id, 0, id_len as i32)?;
        CodecUtil::check_index_header_suffix(input, &BigInt::from(generation).to_str_radix(36))?;

        let lucene_version =
            Version::from_bits(input.read_vint()?, input.read_vint()?, input.read_vint()?)?;

        let index_created_version = input.read_vint()?;
        debug_assert!(index_created_version >= 0);
        if lucene_version.major < index_created_version {
            return Err(LuceneError::corrupt_index(format!(
                "Creation version [{index_created_version}] can't be greater than the version that wrote the segment infos: [{lucene_version}]"
            )));
        }

        if (index_created_version) < min_supported_major_version {
            let reason = format!(
                "This index was initially created with Lucene {}.x while the current version is {} and Lucene only supports reading {}",
                index_created_version,
                *LATEST,
                if min_supported_major_version == *MIN_SUPPORTED_MAJOR {
                    "the current and previous major versions".to_string()
                } else {
                    format!("from version {min_supported_major_version} upwards")
                }
            );
            return Err(LuceneError::index_format_too_old(format!(
                "Format version is not supported (resource {}): {}. This version of Lucene only supports indexes created with release {}.0 and later by default.",
                input, reason, *MIN_SUPPORTED_MAJOR
            )));
        }

        let mut infos = Self::new(index_created_version)?;
        infos.id = Some(id);
        infos.generation = generation;
        infos.last_generation = generation;
        infos.lucene_version = Some(lucene_version);
        if let Err(e) = Self::parse_segment_infos(directory, input, &mut infos, format) {
            prior_error = Some(e);
        }

        if format >= VERSION_74 {
            if let Some(e) = prior_error {
                return Err(CodecUtil::check_footer_with_error(input, e));
            } else {
                CodecUtil::check_footer(input)?;
            }
        } else if let Some(e) = prior_error {
            return Err(e);
        }

        Ok(infos)
    }
    pub fn parse_segment_infos(
        directory: Arc<D>,
        input: &mut impl DataInput,
        infos: &mut SegmentInfos<D>,
        format: i32,
    ) -> Result<()> {
        infos.version = CodecUtil::read_be_long(input)?;
        let counter_value = input.read_vlong()?;
        debug_assert!(counter_value >= 0);
        infos.counter = counter_value;

        let num_segments = CodecUtil::read_be_int(input)?;
        if num_segments < 0 {
            return Err(LuceneError::corrupt_index(format!(
                "Invalid segment count: {num_segments} (resource={input})"
            )));
        }

        if num_segments > 0 {
            // Read minSegmentLuceneVersion
            infos.min_segment_lucene_version = Some(Version::from_bits(
                input.read_vint()?,
                input.read_vint()?,
                input.read_vint()?,
            )?);
        }

        let mut total_docs = 0;

        for _ in 0..num_segments {
            let seg_name = input.read_string()?;
            let mut segment_id = [0u8; StringHelper::ID_LENGTH];
            let segment_id_len = segment_id.len();
            debug_assert!(segment_id_len <= i32::MAX as usize);
            input.read_bytes(&mut segment_id, 0, segment_id_len as i32)?;
            let codec = Self::read_codec(input)?;
            let info = codec.segment_info_format().read(
                directory.clone(),
                &seg_name,
                &segment_id,
                &IO_CONTEXT_DEFAULT,
            )?;
            // info.set_codec(codec)?;

            let max_doc = info.max_doc()?;
            total_docs += max_doc;

            let del_gen = CodecUtil::read_be_long(input)?;
            let del_count = CodecUtil::read_be_int(input)?;
            if del_count > max_doc {
                return Err(LuceneError::corrupt_index(format!(
                    "Invalid deletion count: {del_count} vs maxDoc={max_doc}, (resource={input})"
                )));
            }
            let field_infos_gen = CodecUtil::read_be_long(input)?;
            let dv_gen = CodecUtil::read_be_long(input)?;
            let soft_del_count = CodecUtil::read_be_int(input)?;
            if soft_del_count > max_doc {
                return Err(LuceneError::corrupt_index(format!(
                    "Invalid soft deletion count: {soft_del_count} vs maxDoc={max_doc}, (resource={input})"
                )));
            }

            if soft_del_count + del_count > max_doc {
                return Err(LuceneError::corrupt_index(format!(
                    "Invalid combined deletion count: {} vs maxDoc={}, (resource={})",
                    soft_del_count + del_count,
                    max_doc,
                    input
                )));
            }

            let sci_id = if format > VERSION_74 {
                match input.read_byte()? {
                    1 => {
                        let mut id = [0u8; StringHelper::ID_LENGTH];
                        let id_len = id.len();
                        debug_assert!(id_len <= i32::MAX as usize);
                        input.read_bytes(&mut id, 0, id_len as i32)?;
                        Some(id)
                    },
                    0 => None,
                    marker => {
                        return Err(LuceneError::corrupt_index(format!(
                            "Invalid SegmentCommitInfo ID marker: {marker}"
                        )));
                    },
                }
            } else {
                None
            };

            if let Some(min_version) = &infos.min_segment_lucene_version {
                debug_assert!(info.get_version().is_some());
                if !info
                    .get_version()
                    .as_ref()
                    .unwrap()
                    .on_or_after(min_version)
                {
                    return Err(LuceneError::corrupt_index(format!(
                        "segments file recorded minSegmentLuceneVersion={} but segment={} has older version={} (resource={})",
                        min_version,
                        seg_name,
                        info.get_version().as_ref().unwrap(),
                        input
                    )));
                }
            }
            if infos.index_created_version_major >= 7 {
                if info.get_version().as_ref().unwrap().major < infos.index_created_version_major {
                    return Err(LuceneError::corrupt_index(format!(
                        "segments file recorded indexCreatedVersionMajor={} but segment={} has older version={} (resource={})",
                        infos.index_created_version_major,
                        seg_name,
                        info.get_version().as_ref().unwrap(),
                        input
                    )));
                }

                if info.get_min_version().is_none() {
                    return Err(LuceneError::corrupt_index(format!(
                        "segments infos must record minVersion with indexCreatedVersionMajor={} (resource={})",
                        infos.index_created_version_major, input
                    )));
                }
            }

            let mut si_per_commit = SegmentCommitInfo::new(
                info,
                del_count,
                soft_del_count,
                del_gen,
                field_infos_gen,
                dv_gen,
                sci_id,
            )?;
            si_per_commit.set_field_infos_files(input.read_set_of_strings()?);
            let num_dv_fields = CodecUtil::read_be_int(input)?;
            let dv_update_files = if num_dv_fields == 0 {
                HashMap::new()
            } else {
                let mut map = HashMap::new();
                for _ in 0..num_dv_fields {
                    map.insert(CodecUtil::read_be_int(input)?, input.read_set_of_strings()?);
                }
                map
            };
            si_per_commit.set_doc_values_updates_files(dv_update_files);
            infos.add(si_per_commit)?;
        }
        infos.user_data = input.read_map_of_strings()?;
        // LUCENE-6299: check we are in bounds
        if total_docs > get_actual_max_docs() {
            return Err(LuceneError::corrupt_index(format!(
                "Too many documents: an index cannot exceed {} but readers have total maxDoc={}",
                get_actual_max_docs(),
                total_docs
            )));
        }
        Ok(())
    }

    pub fn read_codec(input: &mut impl DataInput) -> Result<Lucene101Codec> {
        let name = input.read_string()?;
        let codec = get_default_code();
        if codec.get_name() != name {
            return Err(LuceneError::corrupt_index(format!(
                "codec name mismatch: {} != {}",
                codec.get_name(),
                name
            )));
        }
        debug_assert!(LATEST_CODEC.get_name() == codec.get_name());
        Ok(codec)
    }
    /// Find the latest commit (`segments_N` file) and load all
    /// `SegmentCommitInfo`s.
    pub fn read_latest_commit(directory: Arc<D>) -> Result<SegmentInfos<D>> {
        Self::read_latest_commit_with_min_version(directory, *MIN_SUPPORTED_MAJOR)
    }

    /// Find the latest commit (`segments_N` file) with a minimum supported
    /// major version and load all `SegmentCommitInfo`s.
    pub fn read_latest_commit_with_min_version(
        directory: Arc<D>,
        min_supported_major_version: i32,
    ) -> Result<SegmentInfos<D>> {
        let find_segments_file = FindSegmentsFileImpl {
            dir: directory.clone(),
            min_supported_major_version,
        };
        find_segments_file.run()
    }

    fn write_with_directory(&mut self, directory: &impl Directory) -> Result<()> {
        let next_generation = self.get_next_pending_generation();
        let segment_file_name_wrap = IndexFileNames::file_name_from_generation(
            IndexFileNames::PENDING_SEGMENTS,
            "",
            next_generation,
        );
        debug_assert!(segment_file_name_wrap.is_some());
        let segment_file_name = segment_file_name_wrap.unwrap();

        // Always advance the generation on writing
        self.generation = next_generation;

        let mut success = false;
        {
            let result = (|| {
                {
                    let mut segn_output =
                        Some(directory.create_output(&segment_file_name, &IO_CONTEXT_DEFAULT)?);
                    if let Some(ref mut output) = segn_output {
                        self.write(output)?;
                    }
                }
                directory.sync(std::iter::once(&segment_file_name))?;
                success = true;
                Ok(())
            })();
            if let Err(e) = result {
                // Try not to leave a truncated segments_N file in the index
                IOUtils::delete_files_ignoring_exceptions(
                    directory,
                    std::iter::once(&segment_file_name),
                );
                return Err(e);
            }
        }
        if success {
            self.pending_commit = true;
        }

        Ok(())
    }

    /// Write the current `SegmentInfos` to the provided `IndexOutput`.
    ///
    /// # Errors
    ///
    /// Returns a `LuceneError` if there is an issue writing the segment
    /// information.
    pub fn write(&self, out: &mut impl IndexOutput) -> Result<()> {
        let v = BigInt::from(self.generation).to_str_radix(36).to_string();
        CodecUtil::write_index_header(
            out,
            "segments",
            VERSION_CURRENT,
            &StringHelper::random_id(),
            &v,
        )?;
        out.write_vint(LATEST.major)?;
        out.write_vint(LATEST.minor)?;
        out.write_vint(LATEST.bug_fix)?;

        out.write_vint(self.index_created_version_major)?;
        CodecUtil::write_be_long(out, self.version)?;
        out.write_vlong(self.counter)?;
        CodecUtil::write_be_int(out, self.segments.len() as i32)?;

        if self.size() > 0 {
            let mut min_segment_version: Option<Version> = None;
            // We do a separate loop up front so we can write the
            // minSegmentVersion before any SegmentInfo; this makes
            // it cleaner to throw IndexFormatTooOldExc at read time:
            for si_per_commit in self.segments.values() {
                let segment_version = si_per_commit.info.version.clone();
                debug_assert!(segment_version.is_some());
                if min_segment_version.is_none()
                    || !segment_version
                        .as_ref()
                        .unwrap()
                        .on_or_after(min_segment_version.as_ref().unwrap())
                {
                    min_segment_version = segment_version;
                }
            }

            let min_version = min_segment_version.as_ref().unwrap();
            out.write_vint(min_version.major)?;
            out.write_vint(min_version.minor)?;
            out.write_vint(min_version.bug_fix)?;
        }
        for si_per_commit in self.segments.values() {
            let si = &si_per_commit.info;
            if self.index_created_version_major >= 7 && si.min_version.is_none() {
                return Err(LuceneError::illegal_state(format!(
                    "Segments must record minVersion if they have been created on or after Lucene 7: {}",
                    si.name
                )));
            }
            out.write_string(&si.name)?;
            let segment_id = si.get_id();
            let segment_id_len = segment_id.len();
            if segment_id_len != StringHelper::ID_LENGTH {
                return Err(LuceneError::illegal_state(format!(
                    "Cannot write segment: invalid id segment={} id={:?}",
                    si.name, segment_id
                )));
            }
            debug_assert!(segment_id_len <= i32::MAX as usize);
            out.write_bytes_with_len(segment_id, segment_id_len as i32)?;
            out.write_string(LATEST_CODEC.get_name())?;

            CodecUtil::write_be_long(out, si_per_commit.get_del_gen())?;
            let del_count = si_per_commit.get_del_count();
            let max_doc = si.max_doc()?;
            if del_count < 0 || del_count > max_doc {
                return Err(LuceneError::illegal_state(format!(
                    "Cannot write segment: invalid maxDoc segment={} maxDoc={} delCount={}",
                    si.name, max_doc, del_count
                )));
            }
            CodecUtil::write_be_int(out, del_count)?;
            CodecUtil::write_be_long(out, si_per_commit.get_field_infos_gen())?;
            CodecUtil::write_be_long(out, si_per_commit.get_doc_values_gen())?;

            let soft_del_count = si_per_commit.get_soft_del_count();
            if soft_del_count < 0 || soft_del_count > max_doc {
                return Err(LuceneError::illegal_state(format!(
                    "Cannot write segment: invalid maxDoc segment={} maxDoc={} softDelCount={}",
                    si.name, max_doc, soft_del_count
                )));
            }
            CodecUtil::write_be_int(out, soft_del_count)?;

            if let Some(sci_id) = si_per_commit.get_id() {
                out.write_byte(1)?;
                let sci_id_len = sci_id.len();
                debug_assert_eq!(
                    sci_id_len,
                    StringHelper::ID_LENGTH,
                    "Invalid SegmentCommitInfo#id: {sci_id:?}"
                );
                debug_assert!(sci_id_len <= i32::MAX as usize);
                out.write_bytes_range(sci_id, 0, sci_id_len as i32)?;
            } else {
                out.write_byte(0)?;
            }

            out.write_set_of_strings(si_per_commit.get_field_infos_files())?;

            let dv_updates_files = si_per_commit.get_doc_values_updates_files();
            let dv_updates_files_len = dv_updates_files.len();
            debug_assert!(dv_updates_files_len <= i32::MAX as usize);
            CodecUtil::write_be_int(out, dv_updates_files_len as i32)?;
            for (key, value) in dv_updates_files {
                CodecUtil::write_be_int(out, *key)?;
                out.write_set_of_strings(value)?;
            }
        }
        out.write_map_of_strings(&self.user_data)?;
        CodecUtil::write_footer(out)?;

        Ok(())
    }

    pub fn try_clone(&self) -> Result<Self> {
        let mut cloned = Self {
            counter: self.counter,
            version: self.version,
            generation: self.generation,
            last_generation: self.last_generation,
            user_data: self.user_data.clone(),
            segments: HashMap::new(),
            id: self.id,
            lucene_version: self.lucene_version.clone(),
            min_segment_lucene_version: self.min_segment_lucene_version.clone(),
            index_created_version_major: self.index_created_version_major,
            pending_commit: false,
            segments_idx: Vec::new(),
        };

        for segment_commit_info in self.segments.values() {
            // debug_assert!(segment_commit_info.info.codec.is_some());
            cloned.add(segment_commit_info.clone())?;
        }
        Ok(cloned)
    }
    /// Returns the version number when this `SegmentInfos` was generated.
    pub fn get_version(&self) -> i64 {
        self.version
    }

    /// Returns the current generation.
    pub fn get_generation(&self) -> i64 {
        self.generation
    }

    /// Returns the last successfully read or written generation.
    pub fn get_last_generation(&self) -> i64 {
        self.last_generation
    }
    /// Carry over generation numbers from another `SegmentInfos`.
    pub fn update_generation<D1>(&mut self, other: &SegmentInfos<D1>)
    where
        D1: Directory,
    {
        self.last_generation = other.last_generation;
        self.generation = other.generation;
    }

    /// Carry over generation numbers, and version/counter, from another
    /// `SegmentInfos`.
    pub fn update_generation_version_and_counter<D1>(&mut self, other: &SegmentInfos<D1>)
    where
        D1: Directory,
    {
        self.update_generation(other);
        self.version = other.version;
        self.counter = other.counter;
    }

    /// Set the generation to be used for the next commit.
    pub fn set_next_write_generation(&mut self, generation: i64) -> Result<()> {
        if generation < self.generation {
            return Err(LuceneError::illegal_state(format!(
                "Cannot decrease generation to {} from current generation {}",
                generation, self.generation
            )));
        }
        self.generation = generation;
        Ok(())
    }

    /// Rollback a pending commit.
    pub fn rollback_commit(&mut self, directory: &impl Directory) {
        if self.pending_commit {
            self.pending_commit = false;

            // We try to clean up our pending_segments_N

            // Must carefully compute fileName from "generation"
            // since lastGeneration isn't incremented:
            if let Some(pending) = IndexFileNames::file_name_from_generation(
                IndexFileNames::PENDING_SEGMENTS,
                "",
                self.generation,
            ) {
                // Suppress, so we keep throwing the original exception in our
                // caller
                IOUtils::delete_files_ignoring_exceptions(directory, std::iter::once(&pending));
            }
        }
    }
    /// Call this to start a commit. This writes the new segments file, but
    /// writes an invalid checksum at the end, so that it is not visible to
    /// readers. Once this is called, you must call
    /// [`finish_commit`](SegmentInfos::finish_commit) to complete the
    /// commit or [`rollback_commit`](SegmentInfos::rollback_commit) to abort
    /// it.
    ///
    /// Note: [`changed()`](SegmentInfos::changed) should be called prior to
    /// this method if changes have been made to this [`SegmentInfos`] instance.
    pub fn prepare_commit(&mut self, directory: &impl Directory) -> Result<()> {
        if self.pending_commit {
            return Err(LuceneError::illegal_state(
                "prepare_commit was already called",
            ));
        }
        directory.sync_metadata()?;
        self.write_with_directory(directory)?;
        Ok(())
    }

    /// Returns all file names referenced by `SegmentInfo`. The returned
    /// collection is recomputed on each invocation.
    pub fn files(&self, include_segments_file: bool) -> Result<HashSet<String>> {
        let mut files = HashSet::new();
        if include_segments_file && let Some(segment_file_name) = self.get_segments_file_name() {
            files.insert(segment_file_name);
        }
        for segment_commit_info in self.segments.values() {
            files.extend(segment_commit_info.files()?);
        }
        Ok(files)
    }
    /// Returns the committed `segments_N` filename.
    pub fn finish_commit(&mut self, directory: &impl Directory) -> Result<String> {
        if !self.pending_commit {
            return Err(LuceneError::illegal_state("prepare_commit was not called"));
        }

        let mut success_rename_and_sync = false;

        let result = (|| {
            let src = IndexFileNames::file_name_from_generation(
                IndexFileNames::PENDING_SEGMENTS,
                "",
                self.generation,
            )
            .ok_or_else(|| LuceneError::illegal_state("Failed to generate source file name."))?;
            let dest = IndexFileNames::file_name_from_generation(
                IndexFileNames::SEGMENTS,
                "",
                self.generation,
            )
            .ok_or_else(|| {
                LuceneError::illegal_state("Failed to generate destination file name.")
            })?;
            directory.rename(&src, &dest)?;
            directory.sync_metadata()?;
            success_rename_and_sync = true;
            Ok(dest)
        })();

        match result {
            Ok(dest_file) => {
                self.pending_commit = false;
                self.last_generation = self.generation;
                Ok(dest_file)
            },
            Err(e) => {
                if !success_rename_and_sync {
                    // Attempt to roll back the commit if renaming or syncing
                    // failed
                    self.rollback_commit(directory);
                }
                Err(e)
            },
        }
    }
    /// Writes and syncs to the Directory, taking care to remove the segment
    /// file on exception.
    ///
    /// Note: [`changed()`](SegmentInfos::changed) should be called prior to
    /// this method if changes have been made to this [`SegmentInfos`] instance.
    pub fn commit(&mut self, dir: &impl Directory) -> Result<()> {
        self.prepare_commit(dir)?;
        self.finish_commit(dir)?;
        Ok(())
    }
    /// Returns `user_data` saved with this commit.
    pub fn get_user_data(&self) -> &HashMap<String, String> {
        &self.user_data
    }

    /// Sets the commit data.
    pub fn set_user_data(
        &mut self,
        data: Option<HashMap<String, String>>,
        do_increment_version: bool,
    ) {
        if let Some(new_data) = data {
            self.user_data = new_data;
        } else {
            self.user_data = HashMap::new();
        }

        if do_increment_version {
            self.changed();
        }
    }

    /// Replaces all segments in this instance, but keeps generation, version,
    /// counter so that future commits remain write-once.
    pub fn replace(&mut self, other: Self) {
        self.rollback_segment_infos(other.segments.into_values().collect());
        self.last_generation = other.last_generation;
        self.user_data = other.user_data.clone();
    }

    /// Returns the sum of all segment's `max_docs`. Note that this does not
    /// include deletions.
    pub fn total_max_doc(&self) -> Result<i64> {
        let mut count: i64 = 0;
        for segment_commit_info in self.segments.values() {
            count += segment_commit_info.info.max_doc()? as i64;
        }

        // Ensure we don't exceed the actual max document limit.
        debug_assert!(count <= get_actual_max_docs() as i64);
        Ok(count)
    }
    /// Call this before committing if changes have been made to the segments.
    pub fn changed(&mut self) {
        self.version += 1;
    }

    /// Set the version to a new value. The new version must be greater than or
    /// equal to the current version.
    pub fn set_version(&mut self, new_version: i64) -> Result<()> {
        if new_version < self.version {
            return Err(LuceneError::illegal_argument(format!(
                "newVersion (={}) cannot be less than current version (={})",
                new_version, self.version
            )));
        }
        self.version = new_version;
        Ok(())
    }
    // /// Applies all changes caused by committing a merge to this
    // `SegmentInfos`. pub fn apply_merge_changes(
    //     &mut self,
    //     merge: &MergePolicy<D, W>,
    //     drop_segment: bool,
    // ) -> Result<()> {
    //     if self.index_created_version_major >= 7 &&
    // merge.info.info.min_version.is_none() {         return
    // Err(LuceneError::illegal_argument(             "All segments must
    // record the minVersion for indices created on or after Lucene 7"
    //                 .to_string(),
    //         ));
    //     }
    //
    //     let merged_away: HashSet<_> =
    // merge.segments.iter().cloned().collect();     let mut inserted =
    // false;     let mut new_seg_idx = 0;
    //
    //     for seg_idx in 0..self.segments.len() {
    //         debug_assert!(seg_idx >= new_seg_idx);
    //         let info = &self.segments[seg_idx];
    //         if merged_away.contains(info) {
    //             if !inserted && !drop_segment {
    //                 self.segments[new_seg_idx] = merge.info.clone();
    //                 inserted = true;
    //                 new_seg_idx += 1;
    //             }
    //         } else {
    //             self.segments[new_seg_idx] = info.clone();
    //             new_seg_idx += 1;
    //         }
    //     }
    //
    //     // Remove duplicate segments from the list
    //     self.segments.truncate(new_seg_idx);
    //
    //     // If we didn't insert the new segment, check if we should add it to
    // the beginning     if !inserted && !drop_segment {
    //         self.segments.insert(0, merge.info.clone());
    //     }
    //
    //     Ok(())
    // }
    pub fn create_backup_segment_infos(&self) -> Result<Vec<SegmentCommitInfo<D>>> {
        let mut backup_list = Vec::with_capacity(self.segments.len());
        for segment_commit_info in self.segments.values() {
            // debug_assert!(
            //     segment_commit_info.info.codec.is_some(),
            //     "Codec is None for segment {}",
            //     segment_commit_info.info.name
            // );
            backup_list.push(segment_commit_info.clone());
        }
        Ok(backup_list)
    }

    pub fn rollback_segment_infos(&mut self, infos: Vec<SegmentCommitInfo<D>>) {
        self.clear();
        let v: HashMap<String, SegmentCommitInfo<D>> = infos
            .into_iter()
            .map(|sci| (sci.info.get_id_str(), sci))
            .collect();
        self.segments.extend(v);
    }
    /// Returns an iterator over the contained segments in order.
    pub fn iter_mut(&mut self) -> impl Iterator<Item = &mut SegmentCommitInfo<D>> {
        self.segments.values_mut()
    }
    pub fn iter(&self) -> impl Iterator<Item = &SegmentCommitInfo<D>> {
        self.segments.values()
    }
    /// Returns all contained segments as a non-mutable reference to the
    /// internal vector.
    pub fn as_list(&self) -> impl Iterator<Item = &SegmentCommitInfo<D>> {
        self.segments.values()
    }
    /// Returns the number of `SegmentCommitInfo`s.
    pub fn size(&self) -> usize {
        self.segments.len()
    }

    /// Appends the provided `SegmentCommitInfo` to the `segments` list.
    pub fn add(&mut self, si: SegmentCommitInfo<D>) -> Result<()> {
        if self.index_created_version_major >= 7 && si.info.min_version.is_none() {
            return Err(LuceneError::illegal_argument(format!(
                "All segments must record the minVersion for indices created on or after Lucene 7, but minVersion is missing for segment: {si}"
            )));
        }
        let id = si.info.get_id_str();
        self.do_add(id, si)?;
        Ok(())
    }

    pub fn do_add(&mut self, id: String, si: SegmentCommitInfo<D>) -> Result<()> {
        self.segments_idx.push(id.clone());
        match self.segments.insert(id, si) {
            Some(_) => Err(LuceneError::illegal_state(
                "SegmentCommitInfo with the same id already exists",
            )),
            None => Ok(()),
        }
    }

    /// Appends the provided [`SegmentCommitInfo`]s.
    pub fn add_all(&mut self, sis: impl IntoIterator<Item = SegmentCommitInfo<D>>) -> Result<()> {
        for si in sis {
            self.add(si)?;
        }
        Ok(())
    }

    /// Clears all `SegmentCommitInfo`s.
    pub fn clear(&mut self) {
        self.segments.clear();
        self.segments_idx.clear();
    }

    /// Removes the provided `SegmentCommitInfo`.
    pub fn remove(&mut self, si_id: &str) -> bool {
        if let Some(pos) = self.segments_idx.iter().position(|x| x == si_id) {
            self.segments_idx.remove(pos);
        }

        self.segments.remove(si_id).is_some()
    }

    /// Removes the `SegmentCommitInfo` at the provided index.
    pub fn remove_at(&mut self, index: usize) {
        let str = &self.segments_idx[index];
        let _ = self.segments.remove(str);
        self.segments_idx.remove(index);
    }

    /// Returns true if the provided `SegmentCommitInfo` is contained.
    pub fn contains(&self, si_id: &str) -> bool {
        self.segments.contains_key(si_id)
    }

    /// Returns the index of the provided `SegmentCommitInfo`.
    pub fn index_of(&self, si: &SegmentCommitInfo<D>) -> bool {
        self.segments.contains_key(&si.info.get_id_str())
    }

    /// Returns the `Version` of the Lucene commit.
    pub fn get_commit_lucene_version(&self) -> Option<&Version> {
        self.lucene_version.as_ref()
    }

    /// Returns the `Version` of the oldest segment, or `None` if there are no
    /// segments.
    pub fn get_min_segment_lucene_version(&self) -> Option<&Version> {
        self.min_segment_lucene_version.as_ref()
    }

    /// Returns the version major that was used to initially create the index.
    /// This version is set when the index is first created and then never
    /// changes. Older indices report 6 as the creation version.
    pub fn get_index_created_version_major(&self) -> i32 {
        self.index_created_version_major
    }
}

impl<D> fmt::Display for SegmentInfos<D>
where
    D: Directory,
{
    /// Returns a readable description of this segment.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: ", self.get_segments_file_name().unwrap_or_default())?;
        for (i, segment_commit_info) in self.segments.values().enumerate() {
            if i > 0 {
                write!(f, " ")?;
            }
            write!(
                f,
                "{}",
                segment_commit_info.to_string_with_pending_del_count(0)
            )?
        }
        Ok(())
    }
}

pub trait FindSegmentsFile {
    type V;
    type D: Directory;
    fn get_directory_point(&self) -> Arc<Self::D>;
    /// Run doBody on the provided commit.
    fn run_with_commit<IC>(
        &self,
        commit: &impl IndexCommit<Directory = Self::D>,
    ) -> Result<Self::V> {
        if !Arc::ptr_eq(&self.get_directory_point(), &commit.get_directory()) {
            return Err(LuceneError::illegal_state(
                "The specified commit does not match the specified Directory",
            ));
        }
        self.do_body(commit.get_segments_file_name())
    }
    /// Locate the most recent segments file and run doBody on it.
    fn run(&self) -> Result<Self::V> {
        let mut last_gen: i64;
        let mut r#gen: i64 = -1;
        let mut exc: Option<LuceneError> = None;
        // Loop until we succeed in calling doBody() without
        // hitting an IOException.  An IOException most likely
        // means an IW deleted our commit while opening
        // the time it took us to load the now-old infos files
        // (and segments files).  It's also possible it's a
        // true error (corrupt index).  To distinguish these,
        // on each retry we must see "forward progress" on
        // which generation we are trying to load.  If we
        // don't, then the original error is real and we throw
        // it.
        let directory = self.get_directory_point();
        loop {
            last_gen = r#gen;
            let mut files = directory.list_all()?;
            let mut files2 = directory.list_all()?;
            files.sort();
            files2.sort();
            if files != files2 {
                continue;
            }
            r#gen = get_last_commit_generation(&files)?;
            if get_info_stream()?.is_some() {
                message(&format!("directory listing gen={gen}"))?;
            }
            if r#gen == -1 {
                return Err(LuceneError::index_not_found(format!(
                    "No segments* file found in the {}: files: {:?}",
                    directory, files
                )));
            } else if r#gen > last_gen {
                let segment_file_name =
                    IndexFileNames::file_name_from_generation(IndexFileNames::SEGMENTS, "", r#gen)
                        .ok_or_else(|| {
                            LuceneError::illegal_state("Failed to generate segment file name.")
                        })?;
                match self.do_body(&segment_file_name) {
                    Ok(result) => {
                        if get_info_stream()?.is_some() {
                            message(&format!("success on {segment_file_name}")).unwrap_or_default();
                        }
                        return Ok(result);
                    },
                    Err(err) => {
                        if exc.is_none() {
                            exc = Some(err);
                        }

                        if get_info_stream()?.is_some() {
                            message(&format!(
                                "primary Exception on '{}': {}; will retry: gen = {}",
                                segment_file_name,
                                exc.as_ref().unwrap(),
                                r#gen
                            ))
                            .unwrap_or_default();
                        }
                    },
                }
            } else {
                return Err(exc.unwrap_or_else(|| {
                    LuceneError::illegal_state("Unexpected error during FindSegmentsFile::run")
                }));
            }
        }
    }
    /// Sub struct must implement this.
    /// The assumption is an error will be thrown if something goes wrong during the processing that could have been caused by a writer committing.
    fn do_body(&self, segment_file_name: &str) -> Result<Self::V>;
}

pub struct FindSegmentsFileImpl<D>
where
    D: Directory,
{
    pub(crate) dir: Arc<D>,
    pub(crate) min_supported_major_version: i32,
}
impl<D> FindSegmentsFile for FindSegmentsFileImpl<D>
where
    D: Directory,
{
    type V = SegmentInfos<D>;
    type D = D;

    fn get_directory_point(&self) -> Arc<Self::D> {
        self.dir.clone()
    }

    fn do_body(&self, segment_file_name: &str) -> Result<Self::V> {
        SegmentInfos::read_commit_with_file_min_version(
            self.dir.clone(),
            segment_file_name,
            self.min_supported_major_version,
        )
    }
}

/// The version at the time when 8.0 was released.
pub const VERSION_74: i32 = 9;
/// The version that recorded SegmentCommitInfo IDs.
pub const VERSION_86: i32 = 10;
/// Current version of SegmentInfos.
pub(crate) const VERSION_CURRENT: i32 = VERSION_86;
/// Name of the generation reference file name.
pub(crate) const OLD_SEGMENTS_GEN: &str = "segments.gen";
/// Sets the global INFO_STREAM to the given `OutputEnum`.
pub fn set_info_stream(_output: OutputEnum) -> Result<()> {
    // TODO
    Ok(())
}

/// Returns the current global INFO_STREAM as an
/// `Option<Arc<Mutex<OutputEnum>>>`.
pub fn get_info_stream() -> Result<Option<Arc<Mutex<OutputEnum>>>> {
    let info_stream = INFO_STREAM.lock();
    Ok(info_stream.clone())
}

/// Prints a message to the INFO_STREAM if it is set.
/// This function assumes the caller has checked whether INFO_STREAM is `Some`.
pub fn message(msg: &str) -> Result<()> {
    let info_stream = INFO_STREAM.lock();

    if let Some(ref stream) = *info_stream {
        let mut stream = stream.lock();
        writeln!(stream, "SIS: {msg}")
            .map_err(|e| LuceneError::io_with_path("Failed to write", e))?;
    }

    Ok(())
}
/// Get the generation of the most recent commit to the list of index files (N
/// in the segments_N file).
///
/// # Arguments
/// - `files`: A slice of file names to check.
pub fn get_last_commit_generation(files: &[String]) -> Result<i64> {
    let mut max = -1;
    for file in files {
        if file.starts_with(IndexFileNames::SEGMENTS)
                // skipping this file here helps deliver the right exception when opening an old index
                && !file.starts_with(OLD_SEGMENTS_GEN)
        {
            let r#gen = generation_from_segments_file_name(file)?;
            if r#gen > max {
                max = r#gen;
            }
        }
    }
    Ok(max)
}

/// Get the generation of the most recent commit to the index in this directory.
pub fn get_last_commit_generation_from_directory<D: Directory>(directory: &D) -> Result<i64> {
    let files = directory.list_all()?;
    get_last_commit_generation(&files)
}

/// Get the filename of the segments_N file for the most recent commit in the
/// list of index files.
pub fn get_last_commit_segments_file_name(files: &[String]) -> Result<Option<String>> {
    let last_gen = get_last_commit_generation(files)?;
    Ok(IndexFileNames::file_name_from_generation(
        IndexFileNames::SEGMENTS,
        "",
        last_gen,
    ))
}

/// Get the filename of the segments_N file for the most recent commit to the
/// index in this Directory.
pub fn get_last_commit_segments_file_name_from_directory<D: Directory>(
    directory: &D,
) -> Result<Option<String>> {
    let last_gen = get_last_commit_generation_from_directory(directory)?;
    Ok(IndexFileNames::file_name_from_generation(
        IndexFileNames::SEGMENTS,
        "",
        last_gen,
    ))
}
/// Parse the generation off the segment file name and return it.
pub fn generation_from_segments_file_name(file_name: &str) -> Result<i64> {
    if file_name == OLD_SEGMENTS_GEN {
        Err(LuceneError::illegal_argument(format!(
            "\"{}\" is not a valid segment file name since 4.0",
            OLD_SEGMENTS_GEN
        )))
    } else if file_name == IndexFileNames::SEGMENTS {
        Ok(0)
    } else if file_name.starts_with(IndexFileNames::SEGMENTS) {
        let generation_str = &file_name[IndexFileNames::SEGMENTS.len() + 1..];
        match i64::from_str_radix(generation_str, 36) {
            Ok(generation) => Ok(generation),
            Err(_) => Err(LuceneError::number_format(format!(
                "Failed to parse generation from file name: \"{file_name}\""
            ))),
        }
    } else {
        Err(LuceneError::illegal_argument(format!(
            "fileName \"{file_name}\" is not a segments file"
        )))
    }
}
#[cfg(test)]
mod tests {
    use std::collections::{HashMap, HashSet};

    use std::sync::Arc;

    use rand::Rng;

    use crate::core::codecs::segment_info_format::SegmentInfoFormat;
    use crate::core::codecs::{Codec, CodecUtil, get_default_code};
    use crate::core::index::IndexFileNames;
    use crate::core::index::segment_commit_info::SegmentCommitInfo;
    use crate::core::index::segment_info::SegmentInfo;
    use crate::core::index::segment_infos::SegmentInfos;
    use crate::core::index::sort::Sort;
    use crate::core::store::directory::Directory;
    use crate::core::store::dummy::dummy_directory::DummyDirectory;
    use crate::core::store::{DataInput, DataOutput, IOContext, IndexInput};
    use crate::core::util::error::lucene_error::{LuceneError, Result};
    use crate::core::util::{LATEST, LUCENE_10_0_0, LUCENE_11_0_0, StringHelper};
    use crate::test::util::lucene_test_case::lucene_test_case_util::new_directory;
    use crate::test::util::lucene_test_case::lucene_test_case_util::random;
    use crate::test::util::test_util::TestUtil;

    #[allow(dead_code)] // for quick search
    pub struct TestSegmentInfos;
    #[test]
    fn test_illegal_created_version() -> Result<()> {
        // Test for an indexCreatedVersionMajor less than 6
        let result = SegmentInfos::<DummyDirectory>::new(5);
        assert!(result.is_err());
        if let Err(err) = result {
            assert!(
                err.to_string()
                    .contains("indexCreatedVersionMajor must be >= 6")
            );
        }

        // Test for an indexCreatedVersionMajor greater than LATEST.major
        let future_version = LATEST.major + 1;
        let result = SegmentInfos::<DummyDirectory>::new(future_version);
        assert!(result.is_err());
        let expect = format!(
            "indexCreatedVersionMajor is in the future: {}",
            future_version
        );
        if let Err(err) = result {
            assert!(err.to_string().contains(&expect));
        }
        Ok(())
    }
    #[test]
    fn test_versions_no_segments() -> Result<()> {
        let mut random = random();
        let directory = Arc::new(new_directory(&mut random)?);
        let mut sis = SegmentInfos::new(LATEST.major)?;
        sis.commit(directory.as_ref())?;
        sis = SegmentInfos::read_latest_commit(directory.clone())?;
        assert!(sis.get_min_segment_lucene_version().is_none());
        let result = sis.get_commit_lucene_version();
        assert!(result.is_some());
        assert_eq!(*result.unwrap(), *LATEST);
        Ok(())
    }
    #[test]
    fn test_versions_one_segment() -> Result<()> {
        let mut random = random();
        let dir = new_directory(&mut random)?;
        let directory = Arc::new(dir);
        let codec = get_default_code();
        let io_context = IOContext::default_io_context()?;
        let mut sis = SegmentInfos::new(LATEST.major)?;
        let mut info = SegmentInfo::new(
            directory.clone(),
            Some((*LUCENE_11_0_0).clone()),
            Some((*LUCENE_11_0_0).clone()),
            "_0",
            1,
            false,
            false,
            HashMap::new(),
            StringHelper::random_id(),
            HashMap::new(),
            None,
        )?;
        info.set_files(HashSet::new())?;
        codec
            .segment_info_format()
            .write(directory.as_ref(), &mut info, &io_context)?;

        let commit_info =
            SegmentCommitInfo::new(info, 0, 0, -1, -1, -1, Some(StringHelper::random_id()))?;

        sis.add(commit_info)?;
        sis.commit(directory.as_ref())?;

        sis = SegmentInfos::read_latest_commit(directory.clone())?;
        assert_eq!(
            *sis.get_min_segment_lucene_version().unwrap(),
            (*LUCENE_11_0_0).clone()
        );
        assert_eq!(*sis.get_commit_lucene_version().unwrap(), (*LATEST).clone());

        Ok(())
    }

    #[test]
    fn test_versions_two_segments() -> Result<()> {
        let mut random = random();
        let dir = new_directory(&mut random)?;
        let directory = Arc::new(dir);
        let codec = get_default_code();
        let mut sis = SegmentInfos::new(LATEST.major)?;
        let io_context = IOContext::default_io_context()?;
        // First Segment
        let mut info_0 = SegmentInfo::new(
            directory.clone(),
            Some((*LUCENE_11_0_0).clone()),
            Some((*LUCENE_11_0_0).clone()),
            "_0",
            1,
            false,
            false,
            HashMap::new(),
            StringHelper::random_id(),
            HashMap::new(),
            None,
        )?;
        info_0.set_files(HashSet::new())?;
        codec
            .segment_info_format()
            .write(directory.as_ref(), &mut info_0, &io_context)?;

        let commit_info_0 =
            SegmentCommitInfo::new(info_0, 0, 0, -1, -1, -1, Some(StringHelper::random_id()))?;
        let id_0 = commit_info_0.info.get_id_str();
        sis.add(commit_info_0)?;

        // Second Segment
        let mut info_1 = SegmentInfo::new(
            directory.clone(),
            Some((*LUCENE_11_0_0).clone()),
            Some((*LUCENE_11_0_0).clone()),
            "_1",
            1,
            false,
            false,
            HashMap::new(),
            StringHelper::random_id(),
            HashMap::new(),
            None,
        )?;
        info_1.set_files(HashSet::new())?;
        codec
            .segment_info_format()
            .write(directory.as_ref(), &mut info_1, &io_context)?;

        let commit_info_1 =
            SegmentCommitInfo::new(info_1, 0, 0, -1, -1, -1, Some(StringHelper::random_id()))?;
        let id_1 = commit_info_1.info.get_id_str();
        sis.add(commit_info_1)?;
        sis.commit(directory.as_ref())?;

        let commit_info_id_0 = *sis.info(&id_0).unwrap().get_id().unwrap();
        let commit_info_id_1 = *sis.info(&id_1).unwrap().get_id().unwrap();

        // Read back the latest commit
        sis = SegmentInfos::read_latest_commit(directory.clone())?;

        // Verify results
        assert_eq!(
            *sis.get_min_segment_lucene_version().unwrap(),
            (*LUCENE_11_0_0).clone()
        );
        assert_eq!(*sis.get_commit_lucene_version().unwrap(), (*LATEST).clone());
        let actual1 = sis.info(&id_0).unwrap().get_id();
        let actual2 = sis.info(&id_1).unwrap().get_id();
        assert_eq!(
            StringHelper::id_to_string(Option::from(&commit_info_id_0)),
            StringHelper::id_to_string(Option::from(actual1.unwrap()))
        );
        assert_eq!(
            StringHelper::id_to_string(Option::from(&commit_info_id_1)),
            StringHelper::id_to_string(Option::from(actual2.unwrap()))
        );

        Ok(())
    }
    #[test]
    fn test_to_string() -> Result<()> {
        let mut random = random();
        let dir = Arc::new(new_directory(&mut random)?);
        // Diagnostics map
        let diagnostics: HashMap<String, String> = [
            ("key1".to_string(), "value1".to_string()),
            ("key2".to_string(), "value2".to_string()),
        ]
        .iter()
        .cloned()
        .collect();

        // Attributes map
        let attributes: HashMap<String, String> = [
            ("akey1".to_string(), "value1".to_string()),
            ("akey2".to_string(), "value2".to_string()),
        ]
        .iter()
        .cloned()
        .collect();

        // diagnostics X, attributes X
        let si = SegmentInfo::new(
            dir.clone(),
            Some((*LATEST).clone()),
            Some((*LATEST).clone()),
            "TEST",
            10000,
            false,
            false,
            HashMap::new(),
            StringHelper::random_id(),
            HashMap::new(),
            Some(Arc::new(Sort::get_index_order()?)),
        )?;
        assert_eq!(
            format!("TEST({}){}:[indexSort=<doc>]", *LATEST, ":C10000"),
            format!("{}", si)
        );

        // diagnostics O, attributes X
        let si = SegmentInfo::new(
            dir.clone(),
            Some((*LATEST).clone()),
            Some((*LATEST).clone()),
            "TEST",
            10000,
            false,
            false,
            diagnostics.clone(),
            StringHelper::random_id(),
            HashMap::new(),
            Some(Arc::new(Sort::get_index_order()?)),
        )?;
        assert_eq!(
            format!(
                "TEST({}){}:[indexSort=<doc>]:[diagnostics={:?}]",
                *LATEST, ":C10000", diagnostics
            ),
            format!("{}", si)
        );

        // diagnostics X, attributes O
        let si = SegmentInfo::new(
            dir.clone(),
            Some((*LATEST).clone()),
            Some((*LATEST).clone()),
            "TEST",
            10000,
            false,
            false,
            HashMap::new(),
            StringHelper::random_id(),
            attributes.clone(),
            Some(Arc::new(Sort::get_index_order()?)),
        )?;
        assert_eq!(
            format!(
                "TEST({}){}:[indexSort=<doc>]:[attributes={:?}]",
                *LATEST, ":C10000", attributes
            ),
            format!("{}", si)
        );

        // diagnostics O, attributes O
        let si = SegmentInfo::new(
            dir.clone(),
            Some((*LATEST).clone()),
            Some((*LATEST).clone()),
            "TEST",
            10000,
            false,
            false,
            diagnostics.clone(),
            StringHelper::random_id(),
            attributes.clone(),
            Some(Arc::new(Sort::get_index_order()?)),
        )?;
        assert_eq!(
            format!(
                "TEST({}){}:[indexSort=<doc>]:[diagnostics={:?}]:[attributes={:?}]",
                *LATEST, ":C10000", diagnostics, attributes
            ),
            format!("{}", si)
        );
        Ok(())
    }
    #[test]
    fn test_id_changes_on_advance() -> Result<()> {
        let mut random = random();
        let dir = Arc::new(new_directory(&mut random)?);
        let id = StringHelper::random_id();

        let info = SegmentInfo::new(
            dir.clone(),
            Some((*LUCENE_10_0_0).clone()),
            Some((*LUCENE_10_0_0).clone()),
            "_0",
            1,
            false,
            false,
            HashMap::new(),
            StringHelper::random_id(),
            HashMap::new(),
            Some(Arc::new(Sort::get_index_order()?)),
        )?;

        let mut commit_info = SegmentCommitInfo::new(info, 0, 0, -1, -1, -1, Some(id))?;
        assert_eq!(
            StringHelper::id_to_string(Some(&id)),
            StringHelper::id_to_string(commit_info.get_id())
        );

        commit_info.advance_del_gen();
        assert_ne!(
            StringHelper::id_to_string(Some(&id)),
            StringHelper::id_to_string(commit_info.get_id())
        );

        let new_id = *commit_info.get_id().unwrap();
        commit_info.advance_doc_values_gen();
        assert_ne!(
            StringHelper::id_to_string(Some(&new_id)),
            StringHelper::id_to_string(commit_info.get_id())
        );

        let new_id = *commit_info.get_id().unwrap();
        commit_info.advance_field_infos_gen();
        assert_ne!(
            StringHelper::id_to_string(Some(&new_id)),
            StringHelper::id_to_string(commit_info.get_id())
        );

        let clone = commit_info.clone();
        let current_id = *commit_info.get_id().unwrap();
        assert_eq!(
            StringHelper::id_to_string(Some(&current_id)),
            StringHelper::id_to_string(commit_info.get_id())
        );
        assert_eq!(
            StringHelper::id_to_string(Some(&current_id)),
            StringHelper::id_to_string(clone.get_id())
        );

        commit_info.advance_field_infos_gen();
        assert_ne!(
            StringHelper::id_to_string(Some(&current_id)),
            StringHelper::id_to_string(commit_info.get_id())
        );
        assert_eq!(
            StringHelper::id_to_string(Some(&current_id)),
            StringHelper::id_to_string(clone.get_id()),
            "clone changed but shouldn't"
        );

        Ok(())
    }
    #[test]
    fn test_bit_flipped_triggers_corrupt_index_exception() -> Result<()> {
        let mut random = random();
        let dir = Arc::new(new_directory(&mut random)?);
        let codec = get_default_code();
        let mut sis = SegmentInfos::new(LATEST.major)?;
        let io_context = IOContext::default_io_context()?;
        let mut info_0 = SegmentInfo::new(
            dir.clone(),
            Some((*LATEST).clone()),
            Some((*LATEST).clone()),
            "_0",
            1,
            false,
            false,
            HashMap::new(),
            StringHelper::random_id(),
            HashMap::new(),
            None,
        )?;
        info_0.set_files(HashSet::new())?;
        codec
            .segment_info_format()
            .write(dir.as_ref(), &mut info_0, &io_context)?;
        let commit_info_0 =
            SegmentCommitInfo::new(info_0, 0, 0, -1, -1, -1, Some(StringHelper::random_id()))?;
        sis.add(commit_info_0)?;

        // Add second SegmentCommitInfo
        let mut info_1 = SegmentInfo::new(
            dir.clone(),
            Some((*LATEST).clone()),
            Some((*LATEST).clone()),
            "_1",
            1,
            false,
            false,
            HashMap::new(),
            StringHelper::random_id(),
            HashMap::new(),
            None,
        )?;
        info_1.set_files(HashSet::new())?;
        codec
            .segment_info_format()
            .write(dir.as_ref(), &mut info_1, &io_context)?;
        let commit_info_1 =
            SegmentCommitInfo::new(info_1, 0, 0, -1, -1, -1, Some(StringHelper::random_id()))?;
        sis.add(commit_info_1)?;

        sis.commit(dir.as_ref())?;

        // Create a corrupt directory
        let corrupt_dir = Arc::new(new_directory(&mut random)?);
        let mut corrupt = false;
        let io_context = IOContext::read_once_io_context()?;
        {
            let directory = dir.as_ref();
            for file in directory.list_all()? {
                if file.starts_with(IndexFileNames::SEGMENTS) {
                    {
                        let mut input = directory.open_input(&file, &io_context)?;
                        let mut output = corrupt_dir.create_output(&file, &io_context)?;

                        let mut input_length = IndexInput::length(&input);
                        let corrupt_index = TestUtil::next_long(&mut random, 0, input_length - 1);
                        output.copy_bytes(&mut input, corrupt_index)?;

                        let byte = DataInput::read_byte(&mut input)?;
                        let value = random.random_range(0x01..=0xff);
                        let corrupt_byte = byte.wrapping_add(value);
                        output.write_byte(corrupt_byte)?;
                        input_length = IndexInput::length(&input);
                        let file_pointer = input.get_file_pointer();
                        output.copy_bytes(&mut input, input_length - file_pointer)?;
                    }
                    let input = corrupt_dir.open_input(&file, &io_context)?;
                    match CodecUtil::checksum_entire_file(&input) {
                        Ok(_) => {
                            if cfg!(feature = "test_log_verbose") {
                                println!(
                                    "TEST: Altering the file did not update the checksum, aborting..."
                                );
                            }
                            return Ok(());
                        },
                        Err(LuceneError::CorruptIndex(_)) => {
                            // Corruption detected
                        },
                        Err(err) => return Err(err),
                    }
                    corrupt = true;
                } else if file.eq("extra0") {
                    corrupt_dir.copy_from(directory, &file, &file, &io_context)?;
                }
            }
        }

        assert!(corrupt, "No segments file found");

        let result = SegmentInfos::read_latest_commit(corrupt_dir.clone());
        assert!(result.is_err());
        match result {
            Err(LuceneError::CorruptIndex(_))
            | Err(LuceneError::IndexFormatTooOld(_))
            | Err(LuceneError::IndexFormatTooNew(_)) => {},
            _ => {
                unreachable!()
            },
        }

        Ok(())
    }
    #[test]
    fn test_add_diagnostics() -> Result<()> {
        let mut random = random();
        let dir = Arc::new(new_directory(&mut random)?);
        // Diagnostics map
        let diagnostics: HashMap<String, String> = [
            ("key1".to_string(), "value1".to_string()),
            ("key2".to_string(), "value2".to_string()),
        ]
        .iter()
        .cloned()
        .collect();

        // Test adding a new key-value pair
        let mut si = SegmentInfo::new(
            dir.clone(),
            Some((*LATEST).clone()),
            Some((*LATEST).clone()),
            "TEST",
            10000,
            false,
            false,
            diagnostics.clone(),
            StringHelper::random_id(),
            HashMap::new(),
            Some(Arc::new(Sort::get_index_order()?)),
        )?;
        si.add_diagnostics(
            [("key3".to_string(), "value3".to_string())]
                .iter()
                .cloned()
                .collect(),
        );
        let expected_diagnostics: HashMap<String, String> = [
            ("key1".to_string(), "value1".to_string()),
            ("key2".to_string(), "value2".to_string()),
            ("key3".to_string(), "value3".to_string()),
        ]
        .iter()
        .cloned()
        .collect();
        assert_eq!(si.get_diagnostics(), &expected_diagnostics);

        // Test modifying an existing key-value pair
        let mut si = SegmentInfo::new(
            dir.clone(),
            Some((*LATEST).clone()),
            Some((*LATEST).clone()),
            "TEST",
            10000,
            false,
            false,
            diagnostics.clone(),
            StringHelper::random_id(),
            HashMap::new(),
            Some(Arc::new(Sort::get_index_order()?)),
        )?;
        si.add_diagnostics(
            [("key2".to_string(), "foo".to_string())]
                .iter()
                .cloned()
                .collect(),
        );
        let expected_diagnostics: HashMap<String, String> = [
            ("key1".to_string(), "value1".to_string()),
            ("key2".to_string(), "foo".to_string()),
        ]
        .iter()
        .cloned()
        .collect();
        assert_eq!(si.get_diagnostics(), &expected_diagnostics);
        Ok(())
    }
}
