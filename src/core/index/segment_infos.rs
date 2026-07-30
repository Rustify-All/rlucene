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

use parking_lot::Mutex;
use std::sync::LazyLock;

use crate::core::codecs::Codecs;
use crate::core::codecs::codec;
use crate::core::codecs::segment_info_format::SegmentInfoFormat;
use crate::core::codecs::{Codec, CodecUtil};
use crate::core::index::IndexFileNames;
use crate::core::index::index_commit::IndexCommit;

use crate::core::index::codec_reader::CodecReader;
use crate::core::index::index_writer::get_actual_max_docs;
use crate::core::index::merge_policy::OneMerge;
use crate::core::index::segment_commit_info::SegmentCommitInfo;
use crate::core::store::check_sum_index_input::ChecksumIndexInput;
use crate::core::store::directory::Directory;
use crate::core::store::{DataInput, IO_CONTEXT_DEFAULT, IndexOutput};
use crate::core::util::close::{Closeable, CloseableRef};
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::output_enum::OutputEnum;
use crate::core::util::{
  HasIdentity, IOUtils, LATEST, MIN_SUPPORTED_MAJOR, StringHelper, TryIntoInt, Version,
};
use num_bigint::BigInt;
use std::io::{Error, ErrorKind, Write};

static INFO_STREAM: LazyLock<Mutex<Option<OutputEnum>>> = LazyLock::new(|| Mutex::new(None));

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
///   `IndexWriter::setLiveCommitData`.
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
  pub(crate) segments: Vec<SegmentCommitInfo<D>>,
  /// `SegmentCommitInfo`s removed from `segments` but still needed by
  /// concurrent reader-pool work.
  pub(crate) dropped_segment_commit_infos: HashMap<String, SegmentCommitInfo<D>>,
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
  /// Creates a new instance.
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
      segments: Vec::new(),
      dropped_segment_commit_infos: HashMap::new(),
      id: None,
      lucene_version: None,
      min_segment_lucene_version: None,
      index_created_version_major,
      pending_commit: false,
    })
  }
  /// Returns [`SegmentCommitInfo`] at the provided index.
  /// Returns the `SegmentCommitInfo` for the given ID, looking in live
  /// segments first and then in segments retained after being dropped.
  pub fn index_of(&self, seg_id: &str) -> Option<&SegmentCommitInfo<D>> {
    self
      .segments
      .iter()
      .find(|sci| sci.info.get_id_key() == seg_id)
      .or_else(|| self.dropped_segment_commit_infos.get(seg_id))
  }

  /// Returns the live `SegmentCommitInfo` for the given ID.
  pub(crate) fn index_of_live(&self, seg_id: &str) -> Option<&SegmentCommitInfo<D>> {
    self
      .segments
      .iter()
      .find(|sci| sci.info.get_id_key() == seg_id)
  }
  pub fn info(&self, i: usize) -> Option<&SegmentCommitInfo<D>> {
    self.segments.get(i)
  }
  #[cfg(test)]
  pub fn info_idx_mut(&mut self, i: usize) -> Option<&mut SegmentCommitInfo<D>> {
    self.segments.get_mut(i)
  }
  /// Returns the mutable `SegmentCommitInfo` for the given ID, looking in live
  /// segments first and then in segments retained after being dropped.
  pub fn index_of_mut(&mut self, seg_id: &str) -> Option<&mut SegmentCommitInfo<D>> {
    if let Some(index) = self
      .segments
      .iter()
      .position(|sci| sci.info.get_id_key() == seg_id)
    {
      return self.segments.get_mut(index);
    }
    self.dropped_segment_commit_infos.get_mut(seg_id)
  }

  /// Get the segments_N filename in use by this segment infos.
  pub fn get_segments_file_name(&self) -> Option<String> {
    IndexFileNames::file_name_from_generation(IndexFileNames::SEGMENTS, "", self.last_generation)
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
  /// Read a particular `segmentFileName`. This may return an error if a commit
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
  /// [`LuceneError::IndexFormatTooOld`]
  /// will be returned.
  /// Note that this may return an `Err` if a commit is in process.
  pub fn read_commit_with_file_min_version(
    directory: Arc<D>,
    segment_file_name: &str,
    min_supported_major_version: i32,
  ) -> Result<SegmentInfos<D>> {
    let generation = generation_from_segments_file_name(segment_file_name)?;
    let mut input = directory.open_checksum_input(segment_file_name)?;

    let read_result =
      std::panic::catch_unwind(std::panic::AssertUnwindSafe(
        || match SegmentInfos::read_commit_impl(
          directory.clone(),
          &mut input,
          generation,
          min_supported_major_version,
        ) {
          Err(error) => {
            let is_unexpected_file_read_error = match &error {
              LuceneError::Eof(_) | LuceneError::NoSuchFile(_) => true,
              LuceneError::Io { source, .. } | LuceneError::IoWithPath { source, .. } => {
                matches!(
                  source.kind(),
                  ErrorKind::NotFound | ErrorKind::UnexpectedEof
                )
              },
              _ => false,
            };
            if is_unexpected_file_read_error {
              let mut corrupt_index_error = LuceneError::corrupt_index(format!(
                "Unexpected file read error while reading index. (resource={input})"
              ));
              corrupt_index_error.add_suppressed(error);
              Err(corrupt_index_error)
            } else {
              Err(error)
            }
          },
          result => result,
        },
      ));
    let close_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| input.close()));
    IOUtils::use_or_suppress_caught_result(read_result, close_result)
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
    let mut format = -1;
    let read_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<Self> {
      // NOTE: as long as we want to return index_format_too_old (vs
      // corrupt_index), we need to read the magic ourselves.
      let magic = CodecUtil::read_be_int(input)?;
      if magic != CodecUtil::CODEC_MAGIC {
        return Err(LuceneError::index_format_too_old_with_version(
          input,
          magic,
          CodecUtil::CODEC_MAGIC,
          CodecUtil::CODEC_MAGIC,
        ));
      }
      format = CodecUtil::check_header_no_magic(input, "segments", VERSION_74, VERSION_CURRENT)?;

      // Read the ID
      let mut id = [0u8; StringHelper::ID_LENGTH];
      let id_len = id.len();
      debug_assert!(id_len <= i32::MAX as usize);
      input.read_bytes(&mut id, 0, id_len)?;
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

      if index_created_version < min_supported_major_version {
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
        return Err(LuceneError::index_format_too_old(input, reason));
      }

      let mut infos = Self::new(index_created_version)?;
      infos.id = Some(id);
      infos.generation = generation;
      infos.last_generation = generation;
      infos.lucene_version = Some(lucene_version);
      Self::parse_segment_infos(directory, input, &mut infos, format)?;
      Ok(infos)
    }));

    if format >= VERSION_74 {
      match read_result {
        Ok(Ok(infos)) => {
          CodecUtil::check_footer(input)?;
          Ok(infos)
        },
        Ok(Err(error)) => Err(CodecUtil::check_footer_with_error(input, error)),
        Err(payload) => {
          let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            CodecUtil::check_footer(input)
          }));
          std::panic::resume_unwind(payload)
        },
      }
    } else {
      match read_result {
        Ok(result) => result,
        Err(payload) => std::panic::resume_unwind(payload),
      }
    }
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

    let mut total_docs = 0i64;

    for _ in 0..num_segments {
      let seg_name = input.read_string()?;
      let mut segment_id = [0u8; StringHelper::ID_LENGTH];
      let segment_id_len = segment_id.len();
      debug_assert!(segment_id_len <= i32::MAX as usize);
      input.read_bytes(&mut segment_id, 0, segment_id_len)?;
      let codec = Self::read_codec(input)?;
      let mut info = codec.segment_info_format().read(
        directory.clone(),
        &seg_name,
        &segment_id,
        &IO_CONTEXT_DEFAULT,
      )?;
      info.set_codec(codec)?;

      let max_doc = info.max_doc()?;
      total_docs += i64::from(max_doc);

      let del_gen = CodecUtil::read_be_long(input)?;
      let del_count = CodecUtil::read_be_int(input)?;
      if del_count < 0 || del_count > max_doc {
        return Err(LuceneError::corrupt_index(format!(
          "Invalid deletion count: {del_count} vs maxDoc={max_doc}, (resource={input})"
        )));
      }
      let field_infos_gen = CodecUtil::read_be_long(input)?;
      let dv_gen = CodecUtil::read_be_long(input)?;
      let soft_del_count = CodecUtil::read_be_int(input)?;
      if soft_del_count < 0 || soft_del_count > max_doc {
        return Err(LuceneError::corrupt_index(format!(
          "Invalid soft deletion count: {soft_del_count} vs maxDoc={max_doc}, (resource={input})"
        )));
      }

      let combined_del_count = soft_del_count.wrapping_add(del_count);
      if combined_del_count > max_doc {
        return Err(LuceneError::corrupt_index(format!(
          "Invalid combined deletion count: {} vs maxDoc={}, (resource={})",
          combined_del_count, max_doc, input
        )));
      }

      let sci_id = if format > VERSION_74 {
        match input.read_byte()? {
          1 => {
            let mut id = [0u8; StringHelper::ID_LENGTH];
            let id_len = id.len();
            debug_assert!(id_len <= i32::MAX as usize);
            input.read_bytes(&mut id, 0, id_len)?;
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

      let info = Arc::new(info);
      let mut si_per_commit = SegmentCommitInfo::new(
        info.clone(),
        del_count,
        soft_del_count,
        del_gen,
        field_infos_gen,
        dv_gen,
        sci_id,
      );
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

      let segment_version = info.get_version_ref().unwrap();
      if !segment_version.on_or_after(infos.min_segment_lucene_version.as_ref().unwrap()) {
        return Err(LuceneError::corrupt_index(format!(
          "segments file recorded minSegmentLuceneVersion={} but segment={} has older version={} (resource={})",
          infos.min_segment_lucene_version.as_ref().unwrap(),
          info,
          segment_version,
          input
        )));
      }

      if infos.index_created_version_major >= 7
        && segment_version.major < infos.index_created_version_major
      {
        return Err(LuceneError::corrupt_index(format!(
          "segments file recorded indexCreatedVersionMajor={} but segment={} has older version={} (resource={})",
          infos.index_created_version_major, info, segment_version, input
        )));
      }

      if infos.index_created_version_major >= 7 && info.get_min_version_ref().is_none() {
        return Err(LuceneError::corrupt_index(format!(
          "segments infos must record minVersion with indexCreatedVersionMajor={} (resource={})",
          infos.index_created_version_major, input
        )));
      }
    }
    infos.user_data = input.read_map_of_strings()?;
    // LUCENE-6299: check we are in bounds
    if total_docs > i64::from(get_actual_max_docs()) {
      return Err(LuceneError::corrupt_index(format!(
        "Too many documents: an index cannot exceed {} but readers have total maxDoc={}",
        get_actual_max_docs(),
        total_docs
      )));
    }
    Ok(())
  }

  pub fn read_codec(input: &mut impl DataInput) -> Result<Codecs> {
    let name = input.read_string()?;
    match codec::for_name(&name) {
      Err(LuceneError::IllegalArgument(_)) if name.starts_with("Lucene") => {
        Err(LuceneError::illegal_argument(format!(
          "Could not load codec '{name}'. Did you forget to add lucene-backward-codecs.jar?"
        )))
      },
      result => result,
    }
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
    let mut find_segments_file = FindSegmentsFileImpl {
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
    let mut segn_output = None;
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<()> {
      segn_output = Some(directory.create_output(&segment_file_name, &IO_CONTEXT_DEFAULT)?);
      let segn_output = segn_output.as_mut().unwrap();
      self.write(segn_output)?;
      segn_output.close()?;
      let segment_files = vec![segment_file_name.clone()];
      directory.sync(&segment_files)?;
      success = true;
      Ok(())
    }));
    if success {
      self.pending_commit = true;
    } else {
      // We hit an error above; try to close the file but suppress any non-tragic error.
      IOUtils::close_resources_while_handling_error(segn_output.as_mut())?;
      // Try not to leave a truncated segments_N file in the index.
      IOUtils::delete_files_ignoring_exceptions(directory, std::iter::once(&segment_file_name));
    }
    match result {
      Ok(result) => result,
      Err(payload) => std::panic::resume_unwind(payload),
    }
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
    CodecUtil::write_be_int(out, self.segments.len().try_convert()?)?;

    if self.size() > 0 {
      let mut min_segment_version: Option<Version> = None;
      // We do a separate loop up front so we can write the
      // minSegmentVersion before any SegmentInfo; this makes
      // it cleaner to return IndexFormatTooOldExc at read time:
      for si_per_commit in self.segments.iter() {
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
    for si_per_commit in self.segments.iter() {
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
      out.write_bytes_with_len(segment_id, segment_id_len)?;
      out.write_string(si.get_codec()?.get_name())?;

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
        out.write_bytes_range(sci_id, 0, sci_id_len)?;
      } else {
        out.write_byte(0)?;
      }

      out.write_set_of_strings(si_per_commit.get_field_infos_files())?;

      let dv_updates_files = si_per_commit.get_doc_values_updates_files();
      let dv_updates_files_len = dv_updates_files.len();
      CodecUtil::write_be_int(out, dv_updates_files_len.try_convert()?)?;
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
      segments: Vec::new(),
      dropped_segment_commit_infos: self.dropped_segment_commit_infos.clone(),
      id: self.id,
      lucene_version: self.lucene_version.clone(),
      min_segment_lucene_version: self.min_segment_lucene_version.clone(),
      index_created_version_major: self.index_created_version_major,
      pending_commit: self.pending_commit,
    };

    for segment_commit_info in self.segments.iter() {
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
        // Suppress, so we keep returning the original error in our
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
    let size = self.size();
    for i in 0..size {
      let segment_commit_info = self
        .info(i)
        .ok_or_else(|| LuceneError::illegal_state("segment was None"))?;
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

    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<String> {
      let src = IndexFileNames::file_name_from_generation(
        IndexFileNames::PENDING_SEGMENTS,
        "",
        self.generation,
      )
      .ok_or_else(|| LuceneError::illegal_state("Failed to generate source file name."))?;
      let dest =
        IndexFileNames::file_name_from_generation(IndexFileNames::SEGMENTS, "", self.generation)
          .ok_or_else(|| LuceneError::illegal_state("Failed to generate destination file name."))?;
      directory.rename(&src, &dest)?;

      let sync_result =
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| directory.sync_metadata()));
      if matches!(&sync_result, Ok(Ok(()))) {
        success_rename_and_sync = true;
      }
      if !success_rename_and_sync {
        // at this point we already created the file but missed to sync directory let's also
        // remove the
        // renamed file
        IOUtils::delete_files_ignoring_exceptions(directory, std::iter::once(&dest));
      }
      match sync_result {
        Ok(result) => result?,
        Err(payload) => std::panic::resume_unwind(payload),
      }
      Ok(dest)
    }));

    if !success_rename_and_sync {
      // deletes pending_segments_N:
      self.rollback_commit(directory);
    }
    match result {
      Ok(Ok(dest)) => {
        self.pending_commit = false;
        self.last_generation = self.generation;
        Ok(dest)
      },
      Ok(Err(error)) => Err(error),
      Err(payload) => std::panic::resume_unwind(payload),
    }
  }
  /// Writes and syncs to the Directory, taking care to remove the segment
  /// file on error.
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
  pub fn replace(&mut self, other: Self) -> Result<()> {
    self.rollback_segment_infos(other.segments)?;
    self.last_generation = other.last_generation;
    self.user_data = other.user_data.clone();
    Ok(())
  }

  /// Returns the sum of all segment's `max_docs`. Note that this does not
  /// include deletions.
  pub fn total_max_doc(&self) -> Result<i32> {
    let mut count = 0i64;
    for segment_commit_info in self.segments.iter() {
      count += i64::from(segment_commit_info.info.max_doc()?);
    }

    // Ensure we don't exceed the actual max document limit.
    debug_assert!(count <= i64::from(get_actual_max_docs()));
    count.try_convert()
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
  /// Applies all changes caused by committing a merge to this `SegmentInfos`
  pub(crate) fn apply_merge_changes<CR>(
    &mut self,
    merge: &mut OneMerge<D, CR>,
    drop_segment: bool,
  ) -> Result<()>
  where
    CR: CodecReader,
  {
    if self.index_created_version_major >= 7
      && merge
        .info
        .as_ref()
        .and_then(|sci| sci.info.get_min_version())
        .is_none()
    {
      return Err(LuceneError::illegal_argument(
        "All segments must record the minVersion for indices created on or after Lucene 7",
      ));
    }
    let merged_away: HashSet<String> = merge.stat.segments.iter().cloned().collect();

    let mut inserted = false;
    let mut new_segments: Vec<SegmentCommitInfo<D>> = Vec::with_capacity(self.segments.len());

    for info in self.segments.drain(..) {
      let info_id = info.info.get_id_key().to_string();
      if merged_away.contains(&info_id) {
        self.dropped_segment_commit_infos.insert(info_id, info);
        if !inserted && !drop_segment {
          new_segments.push(merge.info.take().unwrap());
          inserted = true;
        }
      } else {
        new_segments.push(info);
      }
    }

    if !inserted && !drop_segment {
      new_segments.insert(0, merge.info.take().unwrap());
    }

    self.segments = new_segments;

    Ok(())
  }

  pub fn create_backup_segment_infos(&self) -> Result<Vec<SegmentCommitInfo<D>>> {
    let mut list = Vec::with_capacity(self.segments.len());
    for segment_commit_info in self.segments.iter() {
      list.push(segment_commit_info.clone());
    }
    Ok(list)
  }

  pub fn rollback_segment_infos(&mut self, infos: Vec<SegmentCommitInfo<D>>) -> Result<()> {
    self.clear();
    debug_assert!(self.segments.is_empty());
    self.add_all(infos)
  }
  pub fn iter_mut(&mut self) -> &mut [SegmentCommitInfo<D>] {
    self.segments.as_mut_slice()
  }
  /// Returns all contained segments as a non-mutable reference to the
  /// internal vector.
  pub fn iter(&self) -> &[SegmentCommitInfo<D>] {
    self.segments.as_slice()
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
    let _id = si.info.get_id_key();
    self.segments.push(si);
    Ok(())
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
    for info in self.segments.drain(..) {
      self
        .dropped_segment_commit_infos
        .insert(info.info.get_id_key().to_string(), info);
    }
  }

  /// Removes the provided `SegmentCommitInfo`.
  pub fn remove_with_id(&mut self, si_id: &str) -> Option<SegmentCommitInfo<D>> {
    let idx = self
      .segments
      .iter()
      .position(|sci| sci.info.get_id_key() == si_id)?;
    let info = self.segments.remove(idx);
    self
      .dropped_segment_commit_infos
      .insert(info.info.get_id_key().to_string(), info.clone());
    Some(info)
  }

  /// Removes the `SegmentCommitInfo` at the provided index.
  pub fn remove(&mut self, index: usize) -> Option<SegmentCommitInfo<D>> {
    if index >= self.segments.len() {
      return None;
    }
    let info = self.segments.remove(index);
    self
      .dropped_segment_commit_infos
      .insert(info.info.get_id_key().to_string(), info.clone());
    Some(info)
  }

  pub(crate) fn remove_dropped_segment_commit_info(
    &mut self,
    si_id: &str,
  ) -> Option<SegmentCommitInfo<D>> {
    self.dropped_segment_commit_infos.remove(si_id)
  }

  /// Returns true if the provided `SegmentCommitInfo` is contained.
  pub fn contains(&self, si_id: &str) -> bool {
    self.index_of_live(si_id).is_some()
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

  pub fn seg_ids(&self) -> Vec<String> {
    self
      .segments
      .iter()
      .map(|s| s.info.get_id_key().to_string())
      .collect()
  }
}

impl<D> fmt::Display for SegmentInfos<D>
where
  D: Directory,
{
  /// Returns a readable description of this segment.
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    write!(f, "{}: ", self.get_segments_file_name().unwrap_or_default())?;
    for (i, segment_commit_info) in self.segments.iter().enumerate() {
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
  fn run_with_commit(
    &mut self,
    commit: &impl IndexCommit<Directory = Arc<Self::D>>,
  ) -> Result<Self::V> {
    if !self
      .get_directory_point()
      .is_same_identity(&*commit.get_directory())
    {
      return Err(LuceneError::io(Error::other(
        "the specified commit does not match the specified Directory",
      )));
    }
    self.do_body(commit.get_segments_file_name())
  }
  /// Locate the most recent segments file and run doBody on it.
  fn run(&mut self) -> Result<Self::V> {
    let mut last_gen: i64;
    let mut gen_: i64 = -1;
    let mut exc: Option<LuceneError> = None;
    // Loop until we succeed in calling doBody() without
    // hitting an I/O error. An I/O error most likely
    // means an IW deleted our commit while opening
    // the time it took us to load the now-old infos files
    // (and segments files).  It's also possible it's a
    // true error (corrupt index).  To distinguish these,
    // on each retry we must see "forward progress" on
    // which generation we are trying to load.  If we
    // don't, then the original error is real and we return
    // it.
    let directory = self.get_directory_point();
    loop {
      last_gen = gen_;
      let mut files = directory.list_all()?;
      let mut files2 = directory.list_all()?;
      files.sort();
      files2.sort();
      if files != files2 {
        continue;
      }
      gen_ = get_last_commit_generation(&files)?;
      if get_info_stream().is_some() {
        message(&format!("directory listing gen={gen_}"))?;
      }
      if gen_ == -1 {
        return Err(LuceneError::index_not_found(format!(
          "No segments* file found in the {}: files: {:?}",
          directory, files
        )));
      } else if gen_ > last_gen {
        let segment_file_name =
          IndexFileNames::file_name_from_generation(IndexFileNames::SEGMENTS, "", gen_)
            .ok_or_else(|| LuceneError::illegal_state("Failed to generate segment file name."))?;
        match self.do_body(&segment_file_name) {
          Ok(result) => {
            if get_info_stream().is_some() {
              message(&format!("success on {segment_file_name}")).unwrap_or_default();
            }
            return Ok(result);
          },
          Err(err) => {
            let error_message = err.to_string();
            if exc.is_none() {
              exc = Some(err);
            }

            if get_info_stream().is_some() {
              message(&format!(
                "primary Exception on '{}': {}; will retry: gen = {}",
                segment_file_name, error_message, gen_
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
  /// The assumption is an error will be returned if something goes wrong during the processing that could have been caused by a writer committing.
  fn do_body(&mut self, segment_file_name: &str) -> Result<Self::V>;
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

  fn do_body(&mut self, segment_file_name: &str) -> Result<Self::V> {
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
/// Sets the global INFO_STREAM to the given optional `OutputEnum`.
pub fn set_info_stream(output: impl Into<Option<OutputEnum>>) -> Result<()> {
  let mut info_stream = INFO_STREAM.lock();
  *info_stream = output.into();
  Ok(())
}

/// Returns the current global INFO_STREAM under its synchronization guard.
pub fn get_info_stream() -> parking_lot::MutexGuard<'static, Option<OutputEnum>> {
  INFO_STREAM.lock()
}

/// Prints a message to the INFO_STREAM if it is set.
/// This function assumes the caller has checked whether INFO_STREAM is `Some`.
pub fn message(msg: &str) -> Result<()> {
  let mut info_stream = INFO_STREAM.lock();

  if let Some(stream) = info_stream.as_mut() {
    let current_thread = std::thread::current();
    let thread_name = current_thread.name().unwrap_or("unnamed");
    let _ = writeln!(stream, "SIS [{thread_name}]: {msg}");
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
                // skipping this file here helps deliver the right error when opening an old index
                && !file.starts_with(OLD_SEGMENTS_GEN)
    {
      let gen_ = generation_from_segments_file_name(file)?;
      if gen_ > max {
        max = gen_;
      }
    }
  }
  Ok(max)
}

/// Get the generation of the most recent commit to the index in this directory.
pub fn get_last_commit_generation_from_directory<D>(directory: &D) -> Result<i64>
where
  D: Directory,
{
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
pub fn get_last_commit_segments_file_name_from_directory<D>(directory: &D) -> Result<Option<String>>
where
  D: Directory,
{
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
