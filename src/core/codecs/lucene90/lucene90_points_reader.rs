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
use crate::core::codecs::points_reader::PointsReader;
use crate::core::index::segment_read_state::SegmentReadState;
use crate::core::store::IndexInput;
use crate::core::store::directory::Directory;
use crate::core::util::CoreHelper;
use crate::core::util::bkd::bkd_reader::BKDReader;
use crate::core::util::error::lucene_error::Result;

pub struct Lucene90PointsReader<I>
where
    I: IndexInput,
{
    // TODO 填充值
    input: I,
}

impl<I> Lucene90PointsReader<I>
where
    I: IndexInput,
{
    pub fn new<D>(_state: &SegmentReadState<D>) -> Self
    where
        D: Directory,
    {
        todo!()
    }
}

impl<I> Clone for Lucene90PointsReader<I>
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

impl<I> PointsReader for Lucene90PointsReader<I>
where
    I: IndexInput,
{
    fn check_integrity(&self) -> Result<()> {
        todo!()
    }

    type PointValuesType = BKDReader<I>;

    fn get_values(&self, _field: &str) -> Result<Self::PointValuesType> {
        todo!()
    }
}
