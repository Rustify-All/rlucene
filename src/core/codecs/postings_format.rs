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
use crate::core::codecs::fields_consumer::FieldsConsumerEnum;
use crate::core::codecs::fields_producer::FieldsProducerEnum;
use crate::core::index::segment_info::SegmentInfo;
use crate::core::index::segment_read_state::SegmentReadState;
use crate::core::index::segment_write_state::SegmentWriteState;
use crate::core::store::directory::Directory;
use crate::core::util::error::lucene_error::Result;
/// Encodes/decodes terms, postings, and proximity data.
pub trait PostingsFormat {
    /// Writes a new segment
    fn fields_consumer<D1, D2>(
        &self,
        state: &SegmentWriteState<D1>,
        segment_info: &SegmentInfo<D2>,
    ) -> Result<FieldsConsumerEnum<D1::IndexOutput>>
    where
        D1: Directory,
        D2: Directory;
    /// Reads a segment. **NOTE**: by the time this call returns, it must hold open any files it will need
    /// to use; else, those files may be deleted. Additionally, required files may be deleted during
    /// the execution of this call before there is a chance to open them. Under these circumstances an
    /// `IOException` should be returned by the implementation. IO exceptions are expected and will
    /// automatically cause a retry of the segment opening logic with the newly revised segments.
    fn fields_producer<D1: Directory, D2: Directory>(
        &self,
        state: &SegmentReadState<D1>,
        segment_info: &SegmentInfo<D2>,
    ) -> Result<FieldsProducerEnum<D1::IndexInput>>;
}
