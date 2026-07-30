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
use crate::core::codecs::CodecUtil;
use crate::core::codecs::field_infos_format::FieldInfosFormat;
use crate::core::index::IndexFileNames;
use crate::core::index::doc_values_skip_index_type::DocValuesSkipIndexType;
use crate::core::index::doc_values_type::DocValuesType;
use crate::core::index::field_info::FieldInfo;
use crate::core::index::field_infos::FieldInfos;
use crate::core::index::index_options::IndexOptions;
use crate::core::index::segment_info::SegmentInfo;
use crate::core::index::vector_encoding::VectorEncoding;
use crate::core::index::vector_similarity_function::VectorSimilarityFunction;
use crate::core::store::directory::Directory;
use crate::core::store::{DataInput, DataOutput, IOContext, IndexInput};
use crate::core::util::close::{Closeable, CloseableRef};
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::{IOUtils, TryIntoInt};
use std::sync::Arc;

// List of vector similarity functions. This list is defined here, in order
// to avoid an undesirable dependency on the declaration and order of values
// in VectorSimilarityFunction. The list values and order have been chosen to
// match that of VectorSimilarityFunction in, at least, Lucene 9.10. Values
pub(crate) const SIMILARITY_FUNCTIONS: [VectorSimilarityFunction; 4] = [
  VectorSimilarityFunction::Euclidean,
  VectorSimilarityFunction::DotProduct,
  VectorSimilarityFunction::Cosine,
  VectorSimilarityFunction::MaximumInnerProduct,
];

/// Lucene 9.0 Field Infos format.
///
/// Field names are stored in the field info file with the suffix `.fnm`.
///
/// # FieldInfos (`.fnm`) Structure
/// `Header, FieldsCount, <FieldName, FieldNumber, FieldBits, DocValuesBits,
/// DocValuesGen, Attributes, DimensionCount, DimensionNumBytes>^FieldsCount,
/// Footer`
///
/// # Data Types
/// - **Header** → [`CodecUtil::check_index_header`]
/// - **FieldsCount** → [`DataOutput::write_vint`]
/// - **FieldName** → [`DataOutput::write_string`]
/// - **FieldBits, IndexOptions, DocValuesBits** → [`DataOutput::write_byte`]
/// - **FieldNumber, DimensionCount, DimensionNumBytes** →
///   [`DataOutput::write_int`]
/// - **Attributes** → [`DataOutput::write_map_of_strings`]
/// - **DocValuesGen** → [`DataOutput::write_long`]
/// - **Footer** → [`CodecUtil::write_footer`](CodecUtil::write_footer)
///
/// # Field Descriptions
/// - **FieldsCount**: The number of fields in this file.
/// - **FieldName**: Name of the field as a UTF-8 string.
/// - **FieldNumber**: The field's number. Unlike previous versions, fields are
///   explicitly numbered rather than implicitly by order.
/// - **FieldBits**: A byte containing field options:
///   - `0x1`: Term vectors stored.
///   - `0x2`: Norms omitted for the indexed field.
///   - `0x4`: Payloads stored for the indexed field.
/// - **IndexOptions**: A byte containing index options:
///   - `0`: Not indexed.
///   - `1`: Indexed as `DOCS_ONLY`.
///   - `2`: Indexed as `DOCS_AND_FREQS`.
///   - `3`: Indexed as `DOCS_AND_FREQS_AND_POSITIONS`.
///   - `4`: Indexed as `DOCS_AND_FREQS_AND_POSITIONS_AND_OFFSETS`.
/// - **DocValuesBits**: A byte containing per-document value types:
///   - High-order bits represent `norms` options.
///   - Low-order bits represent `DocValues` options:
///     - `0`: No DocValues.
///     - `1`: `NumericDocValues` (`DocValuesType::NUMERIC`).
///     - `2`: `BinaryDocValues` (`DocValuesType::BINARY`).
///     - `3`: `SortedDocValues` (`DocValuesType::SORTED`).
/// - **DocValuesGen**: The generation count of the field's `DocValues`.
///   - `-1`: No `DocValues` updates.
///   - `>0`: Updates stored by `DocValuesFormat`.
/// - **Attributes**: A key-value map of codec-private attributes.
/// - **PointDimensionCount, PointNumBytes**: Non-zero if the field is indexed
///   as points (e.g., using `LongPoint`).
/// - **VectorDimension**: Non-zero if the field is indexed as vectors.
/// - **VectorEncoding**: A byte indicating the encoding of vector values:
///   - `0`: `BYTE` (samples stored as signed bytes).
///   - `1`: `FLOAT32` (samples stored in IEEE 32-bit floating point format).
/// - **VectorSimilarityFunction**: A byte representing the similarity function
///   used:
///   - `0`: `EUCLIDEAN`
///     [`VectorSimilarityFunction::EUCLIDEAN`](VectorSimilarityFunction::Euclidean).
///   - `1`: `DOT_PRODUCT`
///     [`VectorSimilarityFunction::DOT_PRODUCT`](VectorSimilarityFunction::DotProduct).
///   - `2`: `COSINE`
///     [`VectorSimilarityFunction::COSINE`](VectorSimilarityFunction::Cosine).
///   - `3`: `MAXIMUM_INNER_PRODUCT`
///     [`VectorSimilarityFunction::MAXIMUM_INNER_PRODUCT`](VectorSimilarityFunction::MaximumInnerProduct).
///
/// # Experimental
pub struct Lucene94FieldInfosFormat;
impl Lucene94FieldInfosFormat {
  pub const EXTENSION: &'static str = "fnm";
  // Codec header
  pub const CODEC_NAME: &'static str = "Lucene94FieldInfos";
  pub const FORMAT_START: i32 = 0;
  // this doesn't actually change the file format but uses up one more bit in
  // an existing bit pattern
  pub const FORMAT_PARENT_FIELD: i32 = 1;
  pub const FORMAT_DOCVALUE_SKIPPER: i32 = 2;
  pub const FORMAT_CURRENT: i32 = Self::FORMAT_DOCVALUE_SKIPPER;

  // Field flags
  pub const STORE_TERMVECTOR: u8 = 0x1;
  pub const OMIT_NORMS: u8 = 0x2;
  pub const STORE_PAYLOADS: u8 = 0x4;
  pub const SOFT_DELETES_FIELD: u8 = 0x8;
  pub const PARENT_FIELD_FIELD: u8 = 0x10;
  pub const DOCVALUES_SKIPPER: u8 = 0x20;
}
impl Default for Lucene94FieldInfosFormat {
  fn default() -> Self {
    Self::new()
  }
}
impl Lucene94FieldInfosFormat {
  pub fn new() -> Self {
    Lucene94FieldInfosFormat {}
  }
  fn doc_values_byte(doc_values_type: &DocValuesType) -> u8 {
    match doc_values_type {
      DocValuesType::None => 0,
      DocValuesType::Numeric => 1,
      DocValuesType::Binary => 2,
      DocValuesType::Sorted => 3,
      DocValuesType::SortedNumeric => 4,
      DocValuesType::SortedSet => 5,
    }
  }

  fn doc_values_skip_index_byte(skip_index_type: &DocValuesSkipIndexType) -> u8 {
    match skip_index_type {
      DocValuesSkipIndexType::None => 0,
      DocValuesSkipIndexType::Range => 1,
    }
  }

  fn get_doc_values_type<I>(input: &I, b: u8) -> Result<DocValuesType>
  where
    I: IndexInput,
  {
    match DocValuesType::from_repr(b) {
      Some(dv) => Ok(dv),
      None => Err(LuceneError::corrupt_index(format!(
        "invalid docvalues byte: {b} (resource={input})"
      ))),
    }
  }

  fn get_doc_values_skip_index_type<I>(input: &I, b: u8) -> Result<DocValuesSkipIndexType>
  where
    I: IndexInput,
  {
    match DocValuesSkipIndexType::from_repr(b) {
      Some(dv) => Ok(dv),
      None => Err(LuceneError::corrupt_index(format!(
        "invalid docvaluesskipindex byte: {b} (resource={input}) "
      ))),
    }
  }

  fn get_vector_encoding<I>(input: &I, b: u8) -> Result<VectorEncoding>
  where
    I: IndexInput,
  {
    match VectorEncoding::from_repr(b) {
      Some(ve) => match ve {
        VectorEncoding::BYTE(_) => Ok(VectorEncoding::BYTE(1)),
        VectorEncoding::FLOAT32(_) => Ok(VectorEncoding::FLOAT32(4)),
      },
      None => Err(LuceneError::corrupt_index(format!(
        "invalid vector encoding: {b} (resource={input})"
      ))),
    }
  }

  fn get_dist_func<I>(input: &I, b: u8) -> Result<VectorSimilarityFunction>
  where
    I: IndexInput,
  {
    SIMILARITY_FUNCTIONS
      .get(b as usize)
      .copied()
      .ok_or_else(|| {
        LuceneError::corrupt_index(format!("invalid distance function: {b} (resource={input})"))
      })
  }
  fn dist_func_to_ord(func: &VectorSimilarityFunction) -> u8 {
    SIMILARITY_FUNCTIONS
      .iter()
      .position(|value| value == func)
      .expect("all VectorSimilarityFunction variants must be listed") as u8
  }

  fn index_options_byte(index_options: &IndexOptions) -> u8 {
    match index_options {
      IndexOptions::None => 0,
      IndexOptions::Docs => 1,
      IndexOptions::DocsAndFreqs => 2,
      IndexOptions::DocsAndFreqsAndPositions => 3,
      IndexOptions::DocsAndFreqsAndPositionsAndOffsets => 4,
    }
  }

  fn get_index_options<I>(input: &I, b: u8) -> Result<IndexOptions>
  where
    I: IndexInput,
  {
    match IndexOptions::from_repr(b) {
      Some(opt) => Ok(opt),
      None => Err(LuceneError::corrupt_index(format!(
        "invalid IndexOptions byte: {b} (resource={input})"
      ))),
    }
  }
  fn vector_encoding_byte(vector_encoding: &VectorEncoding) -> u8 {
    match vector_encoding {
      VectorEncoding::BYTE(_) => 0,
      VectorEncoding::FLOAT32(_) => 1,
    }
  }
}
impl FieldInfosFormat for Lucene94FieldInfosFormat {
  fn read<D>(
    &self,
    directory: &impl Directory,
    segment_info: &SegmentInfo<D>,
    segment_suffix: &str,
    _io_context: &IOContext,
  ) -> Result<FieldInfos>
  where
    D: Directory,
  {
    let file_name =
      IndexFileNames::segment_file_name(&segment_info.name, segment_suffix, Self::EXTENSION);
    let mut input = directory.open_checksum_input(&file_name)?;

    // Wrap the main logic in a closure so we can capture errors for footer
    // checking.
    let mut footer_attempted = false;
    let mut result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(
      || -> Result<FieldInfos> {
        let result = (|| {
          // Check the codec header and determine the file format.
          let format = CodecUtil::check_index_header(
            &mut input,
            Self::CODEC_NAME,
            Self::FORMAT_START,
            Self::FORMAT_CURRENT,
            segment_info.get_id(),
            segment_suffix,
          )?;

          let size = input.read_vint()?;
          let mut infos = Vec::with_capacity(size as usize);

          for _ in 0..size {
            let name = input.read_string()?;
            let field_number = input.read_vint()?;
            if field_number < 0 {
              return Err(LuceneError::corrupt_index(format!(
                "invalid field number for field: {name}, fieldNumber={field_number} (resource={input})"
              )));
            }
            let bits = input.read_byte()?;
            let store_term_vector = (bits & Self::STORE_TERMVECTOR) != 0;
            let omit_norms = (bits & Self::OMIT_NORMS) != 0;
            let store_payloads = (bits & Self::STORE_PAYLOADS) != 0;
            let is_soft_deletes_field = (bits & Self::SOFT_DELETES_FIELD) != 0;
            let is_parent_field = if format >= Self::FORMAT_PARENT_FIELD {
              (bits & Self::PARENT_FIELD_FIELD) != 0
            } else {
              false
            };

            if (bits & 0xC0) != 0 {
              return Err(LuceneError::corrupt_index(format!(
                "unused bits are set \"{bits:b}\""
              )));
            }
            if format < Self::FORMAT_PARENT_FIELD && (bits & 0xF0) != 0 {
              return Err(LuceneError::corrupt_index(format!(
                "parent field bit is set but shouldn't \"{bits:b}\""
              )));
            }
            if format < Self::FORMAT_DOCVALUE_SKIPPER && (bits & Self::DOCVALUES_SKIPPER) != 0 {
              return Err(LuceneError::corrupt_index(format!(
                "doc values skipper bit is set but shouldn't \"{bits:b}\""
              )));
            }

            let index_options_ord = input.read_byte()?;
            let doc_values_type_ord = input.read_byte()?;
            let index_options = Self::get_index_options(&input, index_options_ord)?;
            let doc_values_type = Self::get_doc_values_type(&input, doc_values_type_ord)?;
            let doc_values_skip_index = if format >= Self::FORMAT_DOCVALUE_SKIPPER {
              let doc_values_skip_index_type_ord = input.read_byte()?;
              Self::get_doc_values_skip_index_type(&input, doc_values_skip_index_type_ord)?
            } else {
              DocValuesSkipIndexType::None
            };
            let dv_gen = input.read_long()?;
            let attributes = input.read_map_of_strings()?;
            let point_data_dimension_count = input.read_vint()?.try_convert()?;
            let (point_index_dimension_count, point_num_bytes) = if point_data_dimension_count != 0
            {
              (
                input.read_vint()?.try_convert()?,
                input.read_vint()?.try_convert()?,
              )
            } else {
              (point_data_dimension_count, 0)
            };
            let vector_dimension = input.read_vint()?;
            let vector_encoding_ord = input.read_byte()?;
            let vector_dist_func_ord = input.read_byte()?;
            let vector_encoding = Self::get_vector_encoding(&input, vector_encoding_ord)?;
            let vector_dist_func = Self::get_dist_func(&input, vector_dist_func_ord)?;
            let field_info = FieldInfo::new(
              name,
              field_number,
              store_term_vector,
              omit_norms,
              store_payloads,
              index_options,
              doc_values_type,
              doc_values_skip_index,
              dv_gen,
              attributes,
              point_data_dimension_count,
              point_index_dimension_count,
              point_num_bytes,
              vector_dimension,
              vector_encoding,
              vector_dist_func,
              is_soft_deletes_field,
              is_parent_field,
            )?;
            field_info.check_consistency()?;
            infos.push(Arc::new(field_info));
          }
          Ok(infos)
        })();
        footer_attempted = true;
        match result {
          Ok(infos) => {
            CodecUtil::check_footer(&mut input)?;
            FieldInfos::new(infos)
          },
          Err(error) => Err(CodecUtil::check_footer_with_error(&mut input, error)),
        }
      },
    ));
    let footer_error = if let Err(payload) = &result
      && !footer_attempted
    {
      let error =
        LuceneError::tragedy_from_panic("panic while reading field infos", payload.as_ref());
      Some(CodecUtil::check_footer_with_error(&mut input, error))
    } else {
      None
    };
    if let Some(error @ LuceneError::CorruptIndex(_)) = footer_error {
      result = Ok(Err(error));
    }
    let close_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| input.close()));
    IOUtils::use_or_suppress_caught_result(result, close_result)
  }

  fn write<D>(
    &self,
    directory: &impl Directory,
    segment_info: &SegmentInfo<D>,
    segment_suffix: &str,
    infos: &FieldInfos,
    io_context: &IOContext,
  ) -> Result<()>
  where
    D: Directory,
  {
    let file_name =
      IndexFileNames::segment_file_name(&segment_info.name, segment_suffix, Self::EXTENSION);
    let mut output = directory.create_output(&file_name, io_context)?;

    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<()> {
      CodecUtil::write_index_header(
        &mut output,
        Self::CODEC_NAME,
        Self::FORMAT_CURRENT,
        segment_info.get_id(),
        segment_suffix,
      )?;

      output.write_vint(infos.size() as i32)?;

      for fi in infos.iter() {
        fi.check_consistency()?;

        output.write_string(&fi.name)?;
        output.write_vint(fi.number)?;

        let mut bits: u8 = 0;
        if fi.has_term_vectors() {
          bits |= Self::STORE_TERMVECTOR;
        }
        if fi.omits_norms() {
          bits |= Self::OMIT_NORMS;
        }
        if fi.has_payloads() {
          bits |= Self::STORE_PAYLOADS;
        }
        if fi.is_soft_deletes_field() {
          bits |= Self::SOFT_DELETES_FIELD;
        }
        if fi.is_parent_field() {
          bits |= Self::PARENT_FIELD_FIELD;
        }
        output.write_byte(bits)?;

        output.write_byte(Self::index_options_byte(fi.get_index_options()))?;

        output.write_byte(Self::doc_values_byte(fi.get_doc_values_type()))?;
        output.write_byte(Self::doc_values_skip_index_byte(
          fi.doc_values_skip_index_type(),
        ))?;

        output.write_long(fi.get_doc_values_gen())?;
        output.write_map_of_strings(&fi.attributes().lock().attributes)?;

        output.write_vint(fi.get_point_dimension_count() as i32)?;
        if fi.get_point_dimension_count() != 0 {
          output.write_vint(fi.get_point_index_dimension_count() as i32)?;
          output.write_vint(fi.get_point_num_bytes() as i32)?;
        }
        output.write_vint(fi.get_vector_dimension())?;
        output.write_byte(Self::vector_encoding_byte(fi.get_vector_encoding()))?;
        output.write_byte(Self::dist_func_to_ord(fi.get_vector_similarity_function()))?;
      }

      CodecUtil::write_footer(&mut output)
    }));
    let close_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| output.close()));
    IOUtils::use_or_suppress_caught_result(result, close_result)
  }
}
