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
use crate::core::analysis::char_array_set::CharArraySet;
use crate::core::analysis::filtering_token_filter::{
    FilteringTokenFilter, FilteringTokenFilterBase,
};
use crate::core::analysis::token_attributes::char_term_attribute::CharTermAttribute;
use crate::core::analysis::token_stream::TokenStream;
use crate::core::util::attribute_source::Attributes;

pub struct StopFilter {
    stop_words: CharArraySet,
}
impl StopFilter {
    pub fn new<T>(input: T, stop_words: CharArraySet) -> FilteringTokenFilter<T, StopFilter>
    where
        T: TokenStream,
    {
        let v = StopFilter { stop_words };
        FilteringTokenFilter::new(input, v)
    }
}

impl FilteringTokenFilterBase for StopFilter {
    fn accept(&self, att: &Attributes) -> bool {
        debug_assert!(att.length() <= i32::MAX as usize);
        let length = att.length() as i32;
        !self.stop_words.contains_key(att.buffer(), 0, length)
    }
}
