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
use crate::core::analysis::token_attributes::bytes_term_attribute::BytesTermAttribute;
use crate::core::analysis::token_attributes::term_to_bytes_ref_attribute::TermToBytesRefAttribute;
use crate::core::index::BytesRef;
use crate::core::util::attribute::Attribute;
use crate::core::util::attribute_impl::AttributeImpl;
use std::borrow::Cow;
use std::hash::{Hash, Hasher};

/// Implementation class for BytesTermAttribute.
pub struct BytesTermAttributeImpl {
    bytes: Option<BytesRef<Vec<u8>>>,
}
impl Default for BytesTermAttributeImpl {
    fn default() -> Self {
        Self::new()
    }
}

impl BytesTermAttributeImpl {
    pub fn new() -> Self {
        Self { bytes: None }
    }
}

impl Attribute for BytesTermAttributeImpl {}

impl Clone for BytesTermAttributeImpl {
    fn clone(&self) -> Self {
        let mut c = BytesTermAttributeImpl::new();
        self.copy_to(&mut c);
        c
    }
}

impl AttributeImpl for BytesTermAttributeImpl {
    fn clear(&mut self) {
        let _ = self.bytes.take();
    }

    type AttributeImpl = BytesTermAttributeImpl;

    fn copy_to(&self, other: &mut Self::AttributeImpl) {
        match self.bytes {
            Some(ref bytes) => other.bytes = Some(BytesRef::deep_copy_of(bytes)),
            None => other.bytes = None,
        }
    }
}

impl TermToBytesRefAttribute for BytesTermAttributeImpl {
    fn get_bytes_ref(&mut self) -> Option<Cow<'_, BytesRef<Vec<u8>>>> {
        self.bytes.as_ref().map(Cow::Borrowed)
    }
}

impl BytesTermAttribute for BytesTermAttributeImpl {
    fn set_bytes_ref(&mut self, bytes: BytesRef<Vec<u8>>) {
        self.bytes = Some(bytes);
    }
}
impl Hash for BytesTermAttributeImpl {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.bytes.hash(state);
    }
}
impl PartialEq for BytesTermAttributeImpl {
    fn eq(&self, other: &Self) -> bool {
        self.bytes == other.bytes
    }
}

#[cfg(test)]
mod tests {
    use crate::core::analysis::token_attributes::bytes_term_attribute::BytesTermAttribute;
    use crate::core::analysis::token_attributes::bytes_term_attribute_impl::BytesTermAttributeImpl;
    use crate::core::analysis::token_attributes::term_to_bytes_ref_attribute::TermToBytesRefAttribute;
    use crate::core::index::BytesRef;
    use crate::core::util::attribute_impl::AttributeImpl;
    use crate::core::util::error::lucene_error::Result;
    use std::hash::{DefaultHasher, Hash, Hasher};
    #[allow(dead_code)]
    struct TestBytesRefAttImpl;
    #[test]
    fn test_copy_to() -> Result<()> {
        let mut t = BytesTermAttributeImpl::new();
        let mut copy = assert_copy_is_equal(&t)?;

        // first do empty
        assert_eq!(t.get_bytes_ref(), copy.get_bytes_ref());
        assert!(copy.get_bytes_ref().is_none());

        // now after setting it
        t.set_bytes_ref(BytesRef::from_string("hello"));
        copy = assert_copy_is_equal(&t)?;
        assert_eq!(t.get_bytes_ref(), copy.get_bytes_ref());
        // no need check same instance

        Ok(())
    }
    fn assert_copy_is_equal(att: &BytesTermAttributeImpl) -> Result<BytesTermAttributeImpl> {
        let mut copy = BytesTermAttributeImpl::new();
        att.copy_to(&mut copy);
        assert!(att == &copy, "Copied instance must be equal");

        let mut h1 = DefaultHasher::new();
        att.hash(&mut h1);
        let mut h2 = DefaultHasher::new();
        copy.hash(&mut h2);

        assert_eq!(
            h1.finish(),
            h2.finish(),
            "Copied instance's hashcode must be equal"
        );

        Ok(copy)
    }
}
