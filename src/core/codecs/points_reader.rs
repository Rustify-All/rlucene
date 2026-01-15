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
use crate::core::codecs::DefaultPointsFormat;
use crate::core::codecs::points_format::PointsFormat;
use crate::core::index::point_values::{PointValues, PointValuesEnum2};
use crate::core::util::error::lucene_error::Result;
use std::sync::Arc;
/// Abstract API to visit point values.
pub trait PointsReader {
    /// Checks consistency of this reader.
    ///
    /// Note that this may be costly in terms of I/O, e.g. may involve computing
    /// a checksum value against large data files.
    fn check_integrity(&self) -> Result<()>;

    type PointValuesType: PointValues;
    fn get_values(&self, field: &str) -> Result<Option<Self::PointValuesType>>;

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
pub type DefaultPointsReader<I> = <DefaultPointsFormat as PointsFormat>::PointsReader<I>;

macro_rules! either_points_reader {
    ($vis:vis $name:ident { A: $A:ident, B: $B:ident }) => {
        $vis enum $name<$A, $B> {
            A($A),
            B($B),
        }

        impl<$A, $B> PointsReader for $name<$A, $B>
        where
            $A: PointsReader,
            $B: PointsReader,
        {
            fn check_integrity(&self) -> Result<()> {
                match self {
                    Self::A(inner) => inner.check_integrity(),
                    Self::B(inner) => inner.check_integrity(),
                }
            }

            type PointValuesType = PointValuesEnum2<$A::PointValuesType, $B::PointValuesType>;

            fn get_values(&self, field: &str) -> Result<Option<Self::PointValuesType>> {
                match self {
                    Self::A(inner) => inner
                        .get_values(field)
                        .map(|opt| opt.map(PointValuesEnum2::A)),
                    Self::B(inner) => inner
                        .get_values(field)
                        .map(|opt| opt.map(PointValuesEnum2::B)),
                }
            }

            fn get_merge_instance(&self) -> Result<Option<Self>>
            where
                Self: Sized,
            {
                match self {
                    Self::A(inner) => match inner.get_merge_instance()? {
                        Some(value) => Ok(Some(Self::A(value))),
                        None => Ok(None),
                    },
                    Self::B(inner) => match inner.get_merge_instance()? {
                        Some(value) => Ok(Some(Self::B(value))),
                        None => Ok(None),
                    },
                }
            }
        }
    };
}

either_points_reader!(pub PointsReaderEnum2 { A: A, B: B });

impl<T> PointsReader for Arc<T>
where
    T: PointsReader,
{
    fn check_integrity(&self) -> Result<()> {
        (**self).check_integrity()
    }

    type PointValuesType = T::PointValuesType;

    fn get_values(&self, field: &str) -> Result<Option<Self::PointValuesType>> {
        (**self).get_values(field)
    }

    fn get_merge_instance(&self) -> Result<Option<Self>>
    where
        Self: Sized,
    {
        let v = match (**self).get_merge_instance()? {
            Some(v) => Arc::new(v),
            None => return Ok(None),
        };
        Ok(Some(v))
    }
}
