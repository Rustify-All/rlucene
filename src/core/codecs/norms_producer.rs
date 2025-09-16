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
use crate::core::codecs::doc_values_enum::norms::Lucene90NormNumericDocValuesEnum;
use crate::core::codecs::lucene90_norms_producer::Lucene90NormsProducer;
use crate::core::index::field_info::FieldInfo;
use crate::core::index::numeric_doc_values::NumericDocValues;
use crate::core::store::IndexInput;
use crate::core::util::CoreHelper;
use crate::core::util::error::lucene_error::Result;
use std::sync::Arc;

/// A trait that produces field normalization values.
pub trait NormsProducer: Clone {
    type NumericDocValues: NumericDocValues;
    /// Returns `NumericDocValues` for the given field.
    ///
    /// The returned instance is not required to be thread-safe:
    /// it will only be used by a single thread.
    ///
    /// Behavior is undefined if the given field does not have norms enabled.
    fn get_norms(&self, field: &Arc<FieldInfo>) -> Result<Self::NumericDocValues>;

    /// Checks consistency of this producer.
    ///
    /// Note: this may be expensive in terms of I/O,
    /// for example it might compute a checksum over large data files.
    fn check_integrity(&self) -> Result<()>;

    /// Returns an instance optimized for merging.
    ///
    /// This instance may only be used from the thread that acquires it.
    ///
    /// By default, this method returns `None`, which indicates that no new
    /// `NormsProducerEnum` is required for merging, and the current instance
    /// should be used directly during merge operations.
    fn get_merge_instance(&self) -> Result<Option<Self>>
    where
        Self: Sized,
    {
        Ok(None)
    }
}

pub enum NormsProducerEnum<I>
where
    I: IndexInput,
{
    Lucene90(Lucene90NormsProducer<I>),
}

impl<I> Clone for NormsProducerEnum<I>
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

impl<I> NormsProducer for NormsProducerEnum<I>
where
    I: IndexInput,
{
    type NumericDocValues = Lucene90NormNumericDocValuesEnum<I>;

    fn get_norms(&self, field: &Arc<FieldInfo>) -> Result<Lucene90NormNumericDocValuesEnum<I>> {
        match self {
            NormsProducerEnum::Lucene90(producer) => producer.get_norms(field),
        }
    }

    fn check_integrity(&self) -> Result<()> {
        match self {
            NormsProducerEnum::Lucene90(producer) => producer.check_integrity(),
        }
    }

    fn get_merge_instance(&self) -> Result<Option<Self>>
    where
        Self: Sized,
    {
        match self {
            NormsProducerEnum::Lucene90(producer) => {
                if let Some(merge_instance) = producer.get_merge_instance()? {
                    Ok(Some(NormsProducerEnum::Lucene90(merge_instance)))
                } else {
                    Ok(None)
                }
            },
        }
    }
}
