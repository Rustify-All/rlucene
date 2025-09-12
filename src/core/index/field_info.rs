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
use crate::core::index::doc_values_skip_index_type::DocValuesSkipIndexType;
use crate::core::index::doc_values_type::DocValuesType;
use crate::core::index::index_options::IndexOptions;
use crate::core::index::point_values::{MAX_INDEX_DIMENSIONS, MAX_NUM_BYTES};
use crate::core::index::vector_encoding::VectorEncoding;
use crate::core::index::vector_similarity_function::VectorSimilarityFunction;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use parking_lot::Mutex;
use std::cmp::Ordering;
use std::collections::HashMap;
use std::sync::atomic::AtomicI64;

///  Access to the Field Info file that describes document fields and whether or
/// not they are indexed.  Each segment has a separate Field Info file. Objects
/// of this struct are thread-safe for multiple  readers, but only one thread
/// can be adding documents at a time, with no other reader or writer
///  threads accessing this object.
/// # Note
/// FieldInfo Implement trait Default for padding using
#[derive(Default)] // for padding using
pub struct FieldInfo {
    /// Field's name
    pub name: String,

    /// Internal field number
    pub number: i32,
    doc_values_type: DocValuesType,
    doc_values_skip_index: DocValuesSkipIndexType,
    omit_norms: bool, // omit norms associated with indexed fields
    pub(crate) index_options: IndexOptions,
    pub(crate) inner: Mutex<Inner>,
    dv_gen: AtomicI64,
    ///  If both of these are positive it means this field indexed points (see
    /// [`PointsFormat`](crate::core::codecs::points_format::PointsFormat)).
    point_dimension_count: i32,
    point_index_dimension_count: i32,
    point_num_bytes: i32,
    // if it is a positive value, it means this field indexes vectors
    vector_dimension: i32,
    vector_encoding: VectorEncoding,
    vector_similarity_function: VectorSimilarityFunction,
    // whether this field is used as the soft-deletes field
    soft_deletes_field: bool,
    is_parent_field: bool,
}
pub struct Inner {
    pub(crate) attributes: HashMap<String, String>,
    store_payloads: bool, /* whether this field stores payloads together
                           * with term positions  */
    // True if any document indexed term vectors
    store_term_vector: bool,
}
/// For padding using
impl Default for Inner {
    fn default() -> Self {
        Inner {
            attributes: HashMap::new(),
            store_payloads: false,
            store_term_vector: false,
        }
    }
}

impl FieldInfo {
    /// Sole constructor.
    #[allow(clippy::too_many_arguments)]
    pub fn new<T>(
        name: T,
        number: i32,
        store_term_vector: bool,
        omit_norms: bool,
        store_payloads: bool,
        index_options: IndexOptions,
        doc_values: DocValuesType,
        doc_values_skip_index: DocValuesSkipIndexType,
        dv_gen: i64,
        attributes: HashMap<String, String>,
        point_dimension_count: i32,
        point_index_dimension_count: i32,
        point_num_bytes: i32,
        vector_dimension: i32,
        vector_encoding: VectorEncoding,
        vector_similarity_function: VectorSimilarityFunction,
        soft_deletes_field: bool,
        is_parent_field: bool,
    ) -> Self
    where
        T: Into<String>,
    {
        let doc_values_type = doc_values;

        let (store_term_vector, store_payloads, omit_norms) = if index_options != IndexOptions::None
        {
            (store_term_vector, store_payloads, omit_norms)
        } else {
            (false, false, false)
        };
        let properties = Mutex::new(Inner {
            attributes,
            store_payloads,
            store_term_vector,
        });

        FieldInfo {
            name: name.into(),
            number,
            doc_values_type,
            doc_values_skip_index,
            omit_norms,
            index_options,
            inner: properties,
            dv_gen: AtomicI64::new(dv_gen),
            point_dimension_count,
            point_index_dimension_count,
            point_num_bytes,
            vector_dimension,
            vector_encoding,
            vector_similarity_function,
            soft_deletes_field,
            is_parent_field,
        }
    }

    /// Check correctness of the FieldInfo options
    ///
    /// # Errors
    ///
    /// Returns `IllegalArgumentException` if some options are incorrect
    pub fn check_consistency(&self) -> Result<()> {
        {
            let properties = self.inner.lock();
            if self.index_options != IndexOptions::None {
                // Cannot store payloads unless positions are indexed
                if self
                    .index_options
                    .cmp(&IndexOptions::DocsAndFreqsAndPositions)
                    == Ordering::Less
                    && properties.store_payloads
                {
                    return Err(LuceneError::illegal_argument(format!(
                        "indexed field '{}' cannot have payloads without positions",
                        self.name
                    )));
                }
            } else {
                if properties.store_term_vector {
                    return Err(LuceneError::illegal_argument(format!(
                        "non-indexed field '{}' cannot store term vectors",
                        self.name
                    )));
                }
                if properties.store_payloads {
                    return Err(LuceneError::illegal_argument(format!(
                        "non-indexed field '{}' cannot store payloads",
                        self.name
                    )));
                }
                if self.omit_norms {
                    return Err(LuceneError::illegal_argument(format!(
                        "non-indexed field '{}' cannot omit norms",
                        self.name
                    )));
                }
            }
        }

        if !self
            .doc_values_skip_index
            .is_compatible_with(self.doc_values_type)
        {
            return Err(LuceneError::illegal_argument(format!(
                "field '{}' cannot have docValuesSkipIndexType={:?} with doc values type {:?}",
                self.name, self.doc_values_skip_index, self.doc_values_type
            )));
        }
        if self.dv_gen.load(std::sync::atomic::Ordering::SeqCst) != -1
            && self.doc_values_type == DocValuesType::None
        {
            return Err(LuceneError::illegal_argument(format!(
                "field '{}' cannot have a docvalues update generation without having docvalues",
                self.name
            )));
        }

        if self.point_dimension_count < 0 {
            return Err(LuceneError::illegal_argument(format!(
                "pointDimensionCount must be >= 0; got {} (field: '{}')",
                self.point_dimension_count, self.name
            )));
        }
        if self.point_index_dimension_count < 0 {
            return Err(LuceneError::illegal_argument(format!(
                "pointIndexDimensionCount must be >= 0; got {} (field: '{}')",
                self.point_index_dimension_count, self.name
            )));
        }
        if self.point_num_bytes < 0 {
            return Err(LuceneError::illegal_argument(format!(
                "pointNumBytes must be >= 0; got {} (field: '{}')",
                self.point_num_bytes, self.name
            )));
        }

        if self.point_dimension_count != 0 && self.point_num_bytes == 0 {
            return Err(LuceneError::illegal_argument(format!(
                "pointNumBytes must be > 0 when pointDimensionCount={} (field: '{}')",
                self.point_dimension_count, self.name
            )));
        }
        if self.point_index_dimension_count != 0 && self.point_dimension_count == 0 {
            return Err(LuceneError::illegal_argument(format!(
                "pointIndexDimensionCount must be 0 when pointDimensionCount=0 (field: '{}')",
                self.name
            )));
        }
        if self.point_num_bytes != 0 && self.point_dimension_count == 0 {
            return Err(LuceneError::illegal_argument(format!(
                "pointDimensionCount must be > 0 when pointNumBytes={} (field: '{}')",
                self.point_num_bytes, self.name
            )));
        }

        if self.vector_dimension < 0 {
            return Err(LuceneError::illegal_argument(format!(
                "vectorDimension must be >=0; got {} (field: '{}')",
                self.vector_dimension, self.name
            )));
        }

        if self.soft_deletes_field && self.is_parent_field {
            return Err(LuceneError::illegal_argument(format!(
                "field can't be used as soft-deletes field and parent document field (field: '{}')",
                self.name
            )));
        }

        Ok(())
    }
    /// Verify that the provided FieldInfo has the same schema as this FieldInfo
    ///
    /// # Errors
    ///
    /// Returns `IllegalArgumentException` if the field schemas are not the same
    pub fn verify_same_schema(&self, other: &FieldInfo) -> Result<()> {
        let field_name = &self.name;

        Self::verify_same_index_options(field_name, &self.index_options, &other.index_options)?;

        if self.index_options != IndexOptions::None {
            Self::verify_same_omit_norms(field_name, self.omit_norms, other.omit_norms)?;
            Self::verify_same_store_term_vectors(
                field_name,
                self.inner.lock().store_term_vector,
                other.inner.lock().store_term_vector,
            )?;
        }

        Self::verify_same_doc_values_type(
            field_name,
            &self.doc_values_type,
            &other.doc_values_type,
        )?;
        Self::verify_same_doc_values_skip_index(
            field_name,
            &self.doc_values_skip_index,
            &other.doc_values_skip_index,
        )?;
        Self::verify_same_points_options(
            field_name,
            self.point_dimension_count,
            self.point_index_dimension_count,
            self.point_num_bytes,
            other.point_dimension_count,
            other.point_index_dimension_count,
            other.point_num_bytes,
        )?;
        Self::verify_same_vector_options(
            field_name,
            self.vector_dimension,
            &self.vector_encoding,
            &self.vector_similarity_function,
            other.vector_dimension,
            &other.vector_encoding,
            &other.vector_similarity_function,
        )?;

        Ok(())
    }

    /// Verify that the provided index options are the same
    pub(crate) fn verify_same_index_options(
        field_name: &str,
        index_options1: &IndexOptions,
        index_options2: &IndexOptions,
    ) -> Result<()> {
        if index_options1 != index_options2 {
            return Err(LuceneError::illegal_argument(format!(
                "cannot change field \"{field_name}\" from index options={index_options1:?} to inconsistent index options={index_options2:?}"
            )));
        }
        Ok(())
    }

    /// Verify that the provided docValues type are the same
    pub(crate) fn verify_same_doc_values_type(
        field_name: &str,
        doc_values_type1: &DocValuesType,
        doc_values_type2: &DocValuesType,
    ) -> Result<()> {
        if doc_values_type1 != doc_values_type2 {
            return Err(LuceneError::illegal_argument(format!(
                "cannot change field \"{field_name}\" from doc values type={doc_values_type1:?} to inconsistent doc values type={doc_values_type2:?}"
            )));
        }
        Ok(())
    }

    /// Verify that the provided docValuesSkipIndex are the same
    pub(crate) fn verify_same_doc_values_skip_index(
        field_name: &str,
        skip_index1: &DocValuesSkipIndexType,
        skip_index2: &DocValuesSkipIndexType,
    ) -> Result<()> {
        if skip_index1 != skip_index2 {
            return Err(LuceneError::illegal_argument(format!(
                "cannot change field \"{field_name}\" from docValuesSkipIndexType={skip_index1:?} to inconsistent docValuesSkipIndexType={skip_index2:?}"
            )));
        }
        Ok(())
    }

    /// Verify that the provided store term vectors options are the same
    pub(crate) fn verify_same_store_term_vectors(
        field_name: &str,
        store_term_vector1: bool,
        store_term_vector2: bool,
    ) -> Result<()> {
        if store_term_vector1 != store_term_vector2 {
            return Err(LuceneError::illegal_argument(format!(
                "cannot change field \"{field_name}\" from storeTermVector={store_term_vector1} to inconsistent storeTermVector={store_term_vector2}"
            )));
        }
        Ok(())
    }

    /// Verify that the provided omitNorms are the same
    pub(crate) fn verify_same_omit_norms(
        field_name: &str,
        omit_norms1: bool,
        omit_norms2: bool,
    ) -> Result<()> {
        if omit_norms1 != omit_norms2 {
            return Err(LuceneError::illegal_argument(format!(
                "cannot change field \"{field_name}\" from omitNorms={omit_norms1} to inconsistent omitNorms={omit_norms2}"
            )));
        }
        Ok(())
    }

    /// Verify that the provided points indexing options are the same
    pub(crate) fn verify_same_points_options(
        field_name: &str,
        point_dimension_count1: i32,
        index_dimension_count1: i32,
        num_bytes1: i32,
        point_dimension_count2: i32,
        index_dimension_count2: i32,
        num_bytes2: i32,
    ) -> Result<()> {
        if point_dimension_count1 != point_dimension_count2
            || index_dimension_count1 != index_dimension_count2
            || num_bytes1 != num_bytes2
        {
            return Err(LuceneError::illegal_argument(format!(
                "cannot change field \"{field_name}\" from points dimensionCount={point_dimension_count1}, indexDimensionCount={index_dimension_count1}, numBytes={num_bytes1} to inconsistent dimensionCount={point_dimension_count2}, indexDimensionCount={index_dimension_count2}, numBytes={num_bytes2}"
            )));
        }
        Ok(())
    }

    /// Verify that the provided vector indexing options are the same
    pub(crate) fn verify_same_vector_options(
        field_name: &str,
        vd1: i32,
        ve1: &VectorEncoding,
        vsf1: &VectorSimilarityFunction,
        vd2: i32,
        ve2: &VectorEncoding,
        vsf2: &VectorSimilarityFunction,
    ) -> Result<()> {
        if vd1 != vd2 || vsf1 != vsf2 || ve1 != ve2 {
            return Err(LuceneError::illegal_argument(format!(
                "cannot change field \"{field_name}\" from vector dimension={vd1}, vector encoding={ve1:?}, vector similarity function={vsf1:?} to inconsistent vector dimension={vd2}, vector encoding={ve2:?}, vector similarity function={vsf2:?}"
            )));
        }
        Ok(())
    }

    /// Record that this field is indexed with points, with the specified number
    /// of dimensions and bytes per dimension.
    pub fn set_point_dimensions(
        &mut self,
        dimension_count: i32,
        index_dimension_count: i32,
        num_bytes: i32,
    ) -> Result<()> {
        if dimension_count <= 0 {
            return Err(LuceneError::illegal_argument(format!(
                "point dimension count must be >= 0; got {} for field=\"{}\"",
                dimension_count, self.name
            )));
        }
        if index_dimension_count > MAX_INDEX_DIMENSIONS {
            return Err(LuceneError::illegal_argument(format!(
                "point index dimension count must be < MAX_INDEX_DIMENSIONS  (= {}); got {} for field=\"{}\"",
                MAX_INDEX_DIMENSIONS, index_dimension_count, self.name
            )));
        }
        if index_dimension_count > dimension_count {
            return Err(LuceneError::illegal_argument(format!(
                "point index dimension count must be <= point dimension count (= {}); got {} for field=\"{}\"",
                dimension_count, index_dimension_count, self.name
            )));
        }
        if num_bytes <= 0 {
            return Err(LuceneError::illegal_argument(format!(
                "point numBytes must be >= 0; got {} for field=\"{}\"",
                num_bytes, self.name
            )));
        }
        if num_bytes > MAX_NUM_BYTES {
            return Err(LuceneError::illegal_argument(format!(
                "point numBytes must be <= MAX_NUM_BYTES  (= {}); got {} for field=\"{}\"",
                MAX_NUM_BYTES, num_bytes, self.name
            )));
        }
        if self.point_dimension_count != 0 && self.point_dimension_count != dimension_count {
            return Err(LuceneError::illegal_argument(format!(
                "cannot change point dimension count from {} to {} for field=\"{}\"",
                self.point_dimension_count, dimension_count, self.name
            )));
        }
        if self.point_index_dimension_count != 0
            && self.point_index_dimension_count != index_dimension_count
        {
            return Err(LuceneError::illegal_argument(format!(
                "cannot change point index dimension count from {} to {} for field=\"{}\"",
                self.point_index_dimension_count, index_dimension_count, self.name
            )));
        }
        if self.point_num_bytes != 0 && self.point_num_bytes != num_bytes {
            return Err(LuceneError::illegal_argument(format!(
                "cannot change point numBytes from {} to {} for field=\"{}\"",
                self.point_num_bytes, num_bytes, self.name
            )));
        }

        self.point_dimension_count = dimension_count;
        self.point_index_dimension_count = index_dimension_count;
        self.point_num_bytes = num_bytes;

        self.check_consistency()
    }
    /// Return point data dimension count
    pub fn get_point_dimension_count(&self) -> i32 {
        self.point_dimension_count
    }

    /// Return point data dimension count
    pub fn get_point_index_dimension_count(&self) -> i32 {
        self.point_index_dimension_count
    }

    /// Return number of bytes per dimension
    pub fn get_point_num_bytes(&self) -> i32 {
        self.point_num_bytes
    }

    /// Returns the number of dimensions of the vector value
    pub fn get_vector_dimension(&self) -> i32 {
        self.vector_dimension
    }

    /// Returns the number of dimensions of the vector value
    pub fn get_vector_encoding(&self) -> &VectorEncoding {
        &self.vector_encoding
    }

    /// Returns the VectorSimilarityFunction for the field
    pub fn get_vector_similarity_function(&self) -> &VectorSimilarityFunction {
        &self.vector_similarity_function
    }

    /// Record that this field is indexed with docvalues, with the specified
    /// type
    pub fn set_doc_values_type(&mut self, doc_values_type: DocValuesType) -> Result<()> {
        if self.doc_values_type != DocValuesType::None
            && doc_values_type != DocValuesType::None
            && self.doc_values_type != doc_values_type
        {
            return Err(LuceneError::illegal_argument(format!(
                "cannot change DocValues type from {:?} to {:?} for field \"{}\"",
                self.doc_values_type, doc_values_type, self.name
            )));
        }
        self.doc_values_type = doc_values_type;
        self.check_consistency()?;
        Ok(())
    }

    /// Returns IndexOptions for the field, or IndexOptions.None if the field is
    /// not indexed
    pub fn get_index_options(&self) -> &IndexOptions {
        &self.index_options
    }

    /// Returns name of this field
    pub fn get_name(&self) -> &str {
        &self.name
    }

    /// Returns the field number
    pub fn get_field_number(&self) -> i32 {
        self.number
    }

    /// Returns DocValuesType of the docValues; this is DocValuesType.None if
    /// the field has no docvalues.
    pub fn get_doc_values_type(&self) -> &DocValuesType {
        &self.doc_values_type
    }

    /// Returns true if this field has a skip index
    pub fn doc_values_skip_index_type(&self) -> &DocValuesSkipIndexType {
        &self.doc_values_skip_index
    }

    /// Sets the docValues generation of this field.
    pub fn set_doc_values_gen(&self, dv_gen: i64) -> Result<()> {
        self.dv_gen
            .store(dv_gen, std::sync::atomic::Ordering::SeqCst);
        self.check_consistency()?;
        Ok(())
    }

    /// Returns the docValues generation of this field, or -1 if no docValues
    /// updates exist for it.
    pub fn get_doc_values_gen(&self) -> i64 {
        self.dv_gen.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// Set store term vectors
    pub fn set_store_term_vectors(&self) -> Result<()> {
        self.inner.lock().store_term_vector = true;
        self.check_consistency()?;
        Ok(())
    }

    /// Set store payloads
    pub fn set_store_payloads(&self) -> Result<()> {
        {
            let mut properties = self.inner.lock();
            if self.index_options >= IndexOptions::DocsAndFreqsAndPositions {
                properties.store_payloads = true;
            }
        }
        self.check_consistency()?;
        Ok(())
    }

    /// Returns true if norms are explicitly omitted for this field
    pub fn omits_norms(&self) -> bool {
        self.omit_norms
    }

    /// Omit norms for this field.
    pub fn set_omits_norms(&mut self) -> Result<()> {
        if self.index_options == IndexOptions::None {
            return Err(LuceneError::illegal_argument(
                "cannot omit norms: this field is not indexed".to_string(),
            ));
        }
        self.omit_norms = true;
        self.check_consistency()?;
        Ok(())
    }

    /// Returns true if this field actually has any norms.
    pub fn has_norms(&self) -> bool {
        self.index_options != IndexOptions::None && !self.omit_norms
    }

    /// Returns true if any payloads exist for this field.
    pub fn has_payloads(&self) -> bool {
        let properties = self.inner.lock();
        properties.store_payloads
    }

    /// Returns true if any term vectors exist for this field.
    pub fn has_term_vectors(&self) -> bool {
        self.inner.lock().store_term_vector
    }

    /// Returns whether any (numeric) vector values exist for this field
    pub fn has_vector_values(&self) -> bool {
        self.vector_dimension > 0
    }

    /// Get a codec attribute value, or None if it does not exist
    pub fn get_attribute(&self, key: &str) -> Option<String> {
        let properties = self.inner.lock();
        properties.attributes.get(key).cloned()
    }

    /// Puts a codec attribute value.
    ///
    /// This is a key-value mapping for the field that the codec can use to
    /// store additional metadata, and will be available to the codec when
    /// reading the segment via `getAttribute(String)`.
    ///
    /// If a value already exists for the key in the field, it will be replaced
    /// with the new value. If the value of the attributes for the same
    /// field is changed between documents, the behavior after merge is
    /// undefined.
    pub fn put_attribute(&self, key: String, value: String) -> Option<String> {
        let mut properties = self.inner.lock();
        properties.attributes.insert(key, value)
    }

    /// Returns internal codec attributes map.
    pub fn attributes(&self) -> &Mutex<Inner> {
        &self.inner
    }

    /// Returns true if this field is configured and used as the soft-deletes
    /// field.
    pub fn is_soft_deletes_field(&self) -> bool {
        self.soft_deletes_field
    }

    /// Returns true if this field is configured and used as the parent document
    /// field.
    pub fn is_parent_field(&self) -> bool {
        self.is_parent_field
    }
}
