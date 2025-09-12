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
use std::fmt::{Display, Formatter};

use crate::core::store::DataInput;
use crate::core::util::error::lucene_error;
use crate::core::util::error::lucene_error::LuceneError;
use crate::core::util::fst_impl::fst::BytesReader;

pub struct DummyBytesReader;

impl DataInput for DummyBytesReader {
    fn read_byte(&mut self) -> lucene_error::Result<u8> {
        Err(LuceneError::unsupported_operation(
            "DummyBytesReader does not support reading bytes",
        ))
    }

    fn read_bytes(&mut self, _b: &mut [u8], _offset: i32, _len: i32) -> lucene_error::Result<()> {
        Err(LuceneError::unsupported_operation(
            "DummyBytesReader does not support reading bytes",
        ))
    }

    fn skip_bytes(&mut self, _num_bytes: i64) -> lucene_error::Result<()> {
        Err(LuceneError::unsupported_operation(
            "DummyBytesReader does not support skipping bytes",
        ))
    }
}

impl Display for DummyBytesReader {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", std::any::type_name::<Self>())
    }
}

impl BytesReader for DummyBytesReader {
    fn get_position(&self) -> i64 {
        0
    }

    fn set_position(&mut self, _pos: i64) {}
}
