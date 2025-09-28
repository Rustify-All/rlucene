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
use crate::core::index::index_sorter::{DocComparatorEnum, IndexSorter, StringSorter};
use crate::core::index::leaf_reader::LeafReader;
use crate::core::search::field_comparator::FieldComparatorEnum;
use crate::core::search::pruning::Pruning;
use crate::core::search::sort_field::{
    IndexSorterEnumSorter, MissingValueEnum, SortField, SortFiledBase,
};
use crate::core::search::sorted_numeric_sort_field::{IndexSorterNumeric, SortedNumericSortField};
use crate::core::search::sorted_set_sort_field::{SortedDocValuesProviderImpl, SortedSetSortField};
use crate::core::store::DataOutput;
use crate::core::util::error::lucene_error::Result;
use std::fmt;
use std::fmt::Display;
use std::hash::Hash;

#[derive(Clone, PartialEq, Eq)]
pub enum SortFieldEnum {
    SortedNumeric(SortedNumericSortField),
    SortedSet(SortedSetSortField),
    Sorter(SortField),
}

impl SortFiledBase for SortFieldEnum {
    fn set_missing_value(&mut self, missing_value: Option<MissingValueEnum>) -> Result<()> {
        match self {
            SortFieldEnum::SortedNumeric(sort_field) => sort_field.set_missing_value(missing_value),
            SortFieldEnum::SortedSet(sort_field) => sort_field.set_missing_value(missing_value),
            SortFieldEnum::Sorter(sort_field) => sort_field.set_missing_value(missing_value),
        }
    }

    fn needs_scores(&self) -> bool {
        match self {
            SortFieldEnum::SortedNumeric(sort_field) => sort_field.needs_scores(),
            SortFieldEnum::SortedSet(sort_field) => sort_field.needs_scores(),
            SortFieldEnum::Sorter(sort_field) => sort_field.needs_scores(),
        }
    }

    type IndexSort = IndexSortEnum;

    fn get_index_sorter(&self) -> Result<Option<Self::IndexSort>> {
        match self {
            SortFieldEnum::SortedNumeric(sort_field) => {
                let sorter = sort_field.get_index_sorter()?;
                Ok(sorter.map(IndexSortEnum::SortedNumeric))
            },
            SortFieldEnum::SortedSet(sort_field) => {
                let sorter = sort_field.get_index_sorter()?;
                Ok(sorter.map(IndexSortEnum::SortedSet))
            },
            SortFieldEnum::Sorter(sort_field) => {
                let sorter = sort_field.get_index_sorter()?;
                Ok(sorter.map(IndexSortEnum::Sorter))
            },
        }
    }

    fn serialize(&self, out: &mut impl DataOutput) -> Result<()> {
        match self {
            SortFieldEnum::SortedNumeric(sort_field) => sort_field.serialize(out),
            SortFieldEnum::SortedSet(sort_field) => sort_field.serialize(out),
            SortFieldEnum::Sorter(sort_field) => sort_field.serialize(out),
        }
    }

    type FieldComparator = FieldComparatorEnum;

    fn get_comparator(&self, num_hits: usize, pruning: Pruning) -> Result<Self::FieldComparator> {
        match self {
            SortFieldEnum::SortedNumeric(sort_field) => {
                sort_field.get_comparator(num_hits, pruning)
            },
            SortFieldEnum::SortedSet(sort_field) => sort_field.get_comparator(num_hits, pruning),
            SortFieldEnum::Sorter(sort_field) => sort_field.get_comparator(num_hits, pruning),
        }
    }
}

impl Display for SortFieldEnum {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SortFieldEnum::SortedNumeric(sort_field) => {
                write!(f, "{sort_field}")
            },
            SortFieldEnum::SortedSet(sort_field) => write!(f, "{sort_field}"),
            SortFieldEnum::Sorter(sort_field) => write!(f, "{sort_field}"),
        }
    }
}

impl Hash for SortFieldEnum {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        match self {
            SortFieldEnum::SortedNumeric(sort_field) => sort_field.hash(state),
            SortFieldEnum::SortedSet(sort_field) => sort_field.hash(state),
            SortFieldEnum::Sorter(sort_field) => sort_field.hash(state),
        }
    }
}

impl From<SortField> for SortFieldEnum {
    fn from(sort_field: SortField) -> Self {
        SortFieldEnum::Sorter(sort_field)
    }
}
impl From<SortedNumericSortField> for SortFieldEnum {
    fn from(sort_field: SortedNumericSortField) -> Self {
        SortFieldEnum::SortedNumeric(sort_field)
    }
}
impl From<SortedSetSortField> for SortFieldEnum {
    fn from(sort_field: SortedSetSortField) -> Self {
        SortFieldEnum::SortedSet(sort_field)
    }
}

pub trait SortFieldVecExt {
    fn push_sort_fields(&mut self, item: impl Into<SortFieldEnum>);
}

impl SortFieldVecExt for Vec<SortFieldEnum> {
    fn push_sort_fields(&mut self, item: impl Into<SortFieldEnum>) {
        self.push(item.into());
    }
}

pub enum IndexSortEnum {
    SortedNumeric(IndexSorterNumeric),
    SortedSet(StringSorter<SortedDocValuesProviderImpl>),
    Sorter(IndexSorterEnumSorter),
}
impl IndexSorter for IndexSortEnum {
    fn get_provider_name(&self) -> &str {
        match self {
            IndexSortEnum::SortedNumeric(sorter) => sorter.get_provider_name(),
            IndexSortEnum::SortedSet(sorter) => sorter.get_provider_name(),
            IndexSortEnum::Sorter(sorter) => sorter.get_provider_name(),
        }
    }

    type DocComparator = DocComparatorEnum;

    fn get_doc_comparator<LR>(&self, leaf_reader: &LR, max_doc: i32) -> Result<Self::DocComparator>
    where
        LR: LeafReader,
    {
        match self {
            IndexSortEnum::SortedNumeric(sorter) => sorter.get_doc_comparator(leaf_reader, max_doc),
            IndexSortEnum::SortedSet(sorter) => Ok(DocComparatorEnum::String(
                sorter.get_doc_comparator(leaf_reader, max_doc)?,
            )),
            IndexSortEnum::Sorter(sorter) => sorter.get_doc_comparator(leaf_reader, max_doc),
        }
    }
}
