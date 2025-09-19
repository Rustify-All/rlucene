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
use crate::core::index::dummy::dummy_impacts_enum::DummyImpactsEnum;
use crate::core::index::dummy::dummy_postings_enum::DummyPostingsEnum;
use crate::core::index::dummy::dummy_term_state_type::DummyTermState;
use crate::core::index::terms_enum::{ReadyPreparedSeekExact, TermsEnum};
use crate::core::util::bytes_ref_iterator::BytesRefIterator;
use crate::core::util::dummy::dummy_attribute_source::DummyAttributeSource;

/// Implements a [`TermsEnum`](TermsEnum) wrapping a provided
/// [`SortedDocValues`](SortedDocValues).
pub struct SortedDocValuesTermsEnum;

impl BytesRefIterator for SortedDocValuesTermsEnum {}

impl TermsEnum for SortedDocValuesTermsEnum {
    type AttributeSource = DummyAttributeSource;
    type PreparedSeekExact<'a> = ReadyPreparedSeekExact;

    type PostingsEnum = DummyPostingsEnum;
    type ImpactsEnum = DummyImpactsEnum;
    type TermState = DummyTermState;
}
