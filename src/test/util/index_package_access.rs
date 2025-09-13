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
use crate::core::index::field_info::FieldInfo;
use crate::core::index::field_infos::build::Builder;
use crate::core::index::field_infos::{FieldInfos, FieldNumbers};
use crate::core::util::error::lucene_error::Result;
use parking_lot::lock_api::Mutex;
use std::sync::Arc;

pub(crate) trait IndexPackageAccess {
    // type CacheKey;
    type FieldInfosBuilder: FieldInfosBuilder;
    // fn new_cache_key(&self) -> Self::CacheKey;
    // fn set_index_writer_max_docs(&mut self, limit: i32);
    fn new_field_infos_builder(
        &self,
        soft_deletes_field_name: Option<String>,
        parent_field_name: Option<String>,
    ) -> Result<Self::FieldInfosBuilder>;
    // fn check_impacts(&self, impacts: Impacts, max: i32);
}
pub(crate) trait FieldInfosBuilder {
    fn add(&mut self, fi: Arc<FieldInfo>) -> Result<&mut Self>;
    fn finish(&mut self) -> Result<FieldInfos>;
}

pub(crate) struct IndexPackageAccessImpl;
impl IndexPackageAccess for IndexPackageAccessImpl {
    type FieldInfosBuilder = FieldInfosBuilderImpl;

    fn new_field_infos_builder(
        &self,
        soft_deletes_field_name: Option<String>,
        parent_field_name: Option<String>,
    ) -> Result<Self::FieldInfosBuilder> {
        FieldInfosBuilderImpl::new(soft_deletes_field_name, parent_field_name)
    }
}

pub(crate) struct FieldInfosBuilderImpl {
    builder: Builder,
}
impl FieldInfosBuilderImpl {
    pub fn new<S, P>(
        soft_deletes_field_name: Option<S>,
        parent_field_name: Option<P>,
    ) -> Result<Self>
    where
        S: Into<String>,
        P: Into<String>,
    {
        let field_number = FieldNumbers::new(soft_deletes_field_name, parent_field_name)?;
        Ok(FieldInfosBuilderImpl {
            builder: Builder::new(Arc::new(Mutex::new(field_number))),
        })
    }
}
impl FieldInfosBuilder for FieldInfosBuilderImpl {
    fn add(&mut self, fi: Arc<FieldInfo>) -> Result<&mut Self> {
        self.builder.add(fi)?;
        Ok(self)
    }

    fn finish(&mut self) -> Result<FieldInfos> {
        self.builder.finish()
    }
}
