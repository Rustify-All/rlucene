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
use crate::core::index::BytesRef;
use crate::core::util::access::ByteSource;
use crate::core::util::error::lucene_error::Result;

/// Completes a prepared seek on its original cursor and target.
pub trait IOBooleanSupplier<T: ?Sized, BS: ByteSource> {
  fn get(&self, cursor: &mut T, target: &BytesRef<BS>) -> Result<bool>;
}

/// Adapts a delegate's supplier to a cursor wrapper.
pub struct MappedBooleanSupplier<T: ?Sized, BS, S> {
  pub(crate) delegate: S,
  pub(crate) get: fn(&S, &mut T, &BytesRef<BS>) -> Result<bool>,
}

impl<T: ?Sized, BS: ByteSource, S> IOBooleanSupplier<T, BS> for MappedBooleanSupplier<T, BS, S> {
  fn get(&self, cursor: &mut T, target: &BytesRef<BS>) -> Result<bool> {
    (self.get)(&self.delegate, cursor, target)
  }
}
