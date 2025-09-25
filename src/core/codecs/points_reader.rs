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
use crate::core::codecs::lucene90_points_reader::Lucene90PointsReader;
use crate::core::index::point_values::PointValues;
use crate::core::store::IndexInput;
use crate::core::util::CoreHelper;
use crate::core::util::bkd::bkd_reader::BKDReader;
use crate::core::util::error::lucene_error::Result;
/// Abstract API to visit point values.
pub trait PointsReader: Clone {
    /// Checks consistency of this reader.
    ///
    /// Note that this may be costly in terms of I/O, e.g. may involve computing
    /// a checksum value against large data files.
    fn check_integrity(&self) -> Result<()>;

    type PointValuesType: PointValues;
    fn get_values(&self, field: &str) -> Result<Self::PointValuesType>;

    /// Returns an instance optimized for merging. This instance may only be
    /// cloned
    /// # Note
    /// Returning None means returning itself.
    fn get_merge_instance(&self) -> Result<Option<Self>>
    where
        Self: Sized,
    {
        Ok(None)
    }
}

pub enum PointsReaderEnum<I>
where
    I: IndexInput,
{
    Lucene90(Lucene90PointsReader<I>),
}

impl<I> Clone for PointsReaderEnum<I>
where
    I: IndexInput,
{
    fn clone(&self) -> Self {
        unreachable!(
            "{} {}",
            std::any::type_name::<Self>(),
            CoreHelper::CLONE_WARRING
        )
    }
}

impl<I> PointsReader for PointsReaderEnum<I>
where
    I: IndexInput,
{
    fn check_integrity(&self) -> Result<()> {
        match self {
            PointsReaderEnum::Lucene90(reader) => reader.check_integrity(),
        }
    }

    type PointValuesType = BKDReader<I>;

    fn get_values(&self, field: &str) -> Result<Self::PointValuesType> {
        match self {
            PointsReaderEnum::Lucene90(reader) => reader.get_values(field),
        }
    }

    fn get_merge_instance(&self) -> Result<Option<Self>>
    where
        Self: Sized,
    {
        match self {
            PointsReaderEnum::Lucene90(reader) => {
                let merge_instance = reader.get_merge_instance()?;
                Ok(merge_instance.map(PointsReaderEnum::Lucene90))
            },
        }
    }
}
