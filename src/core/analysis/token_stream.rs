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
use crate::core::analysis::reader::ReaderEnum;
use crate::core::analysis::token_attributes::packed_token_attribute_impl::PackedTokenAttributeImpl;
use crate::core::util::attribute_source::{AttributeSource, Attributes};
use crate::core::util::error::lucene_error::Result;
pub trait TokenStream {
    fn increment_token(&mut self) -> Result<bool> {
        unreachable!("must be implemented by sub");
    }
    fn end(&mut self) -> Result<()>;
    fn default_end(&mut self) -> Result<()> {
        self.get_attribute_source_mut().end_attributes();
        Ok(())
    }
    fn reset(&mut self) -> Result<()> {
        Ok(())
    }
    fn default_reset(&mut self) -> Result<()> {
        Ok(())
    }
    fn close(&mut self) -> Result<()> {
        Ok(())
    }
    fn get_attribute_source(&self) -> &Attributes;
    fn get_attribute_source_mut(&mut self) -> &mut Attributes;
    fn set_reader(&mut self, _input: ReaderEnum) -> Result<()> {
        Ok(())
    }
    fn set_reader_test_point(&mut self) {}
}
pub fn default_attribute() -> Attributes {
    Attributes::PackedToken(PackedTokenAttributeImpl::new())
}

pub enum Either2TokenStream<A, B> {
    A(A),
    B(B),
}
impl<A, B> TokenStream for Either2TokenStream<A, B>
where
    A: TokenStream,
    B: TokenStream,
{
    fn increment_token(&mut self) -> Result<bool> {
        match self {
            Either2TokenStream::A(a) => a.increment_token(),
            Either2TokenStream::B(b) => b.increment_token(),
        }
    }

    fn end(&mut self) -> Result<()> {
        match self {
            Either2TokenStream::A(a) => a.end(),
            Either2TokenStream::B(b) => b.end(),
        }
    }

    fn default_end(&mut self) -> Result<()> {
        match self {
            Either2TokenStream::A(a) => a.default_end(),
            Either2TokenStream::B(b) => b.default_end(),
        }
    }

    fn reset(&mut self) -> Result<()> {
        match self {
            Either2TokenStream::A(a) => a.reset(),
            Either2TokenStream::B(b) => b.reset(),
        }
    }

    fn default_reset(&mut self) -> Result<()> {
        match self {
            Either2TokenStream::A(a) => a.default_reset(),
            Either2TokenStream::B(b) => b.default_reset(),
        }
    }

    fn close(&mut self) -> Result<()> {
        match self {
            Either2TokenStream::A(a) => a.close(),
            Either2TokenStream::B(b) => b.close(),
        }
    }

    fn get_attribute_source(&self) -> &Attributes {
        match self {
            Either2TokenStream::A(a) => a.get_attribute_source(),
            Either2TokenStream::B(b) => b.get_attribute_source(),
        }
    }

    fn get_attribute_source_mut(&mut self) -> &mut Attributes {
        match self {
            Either2TokenStream::A(a) => a.get_attribute_source_mut(),
            Either2TokenStream::B(b) => b.get_attribute_source_mut(),
        }
    }

    fn set_reader(&mut self, _input: ReaderEnum) -> Result<()> {
        match self {
            Either2TokenStream::A(a) => a.set_reader(_input),
            Either2TokenStream::B(b) => b.set_reader(_input),
        }
    }

    fn set_reader_test_point(&mut self) {
        match self {
            Either2TokenStream::A(a) => a.set_reader_test_point(),
            Either2TokenStream::B(b) => b.set_reader_test_point(),
        }
    }
}

impl<T> TokenStream for &mut T
where
    T: TokenStream + ?Sized,
{
    fn increment_token(&mut self) -> Result<bool> {
        (**self).increment_token()
    }

    fn end(&mut self) -> Result<()> {
        (**self).end()
    }

    fn default_end(&mut self) -> Result<()> {
        (**self).default_end()
    }

    fn reset(&mut self) -> Result<()> {
        (**self).reset()
    }

    fn default_reset(&mut self) -> Result<()> {
        (**self).default_reset()
    }

    fn close(&mut self) -> Result<()> {
        (**self).close()
    }

    fn get_attribute_source(&self) -> &Attributes {
        (**self).get_attribute_source()
    }

    fn get_attribute_source_mut(&mut self) -> &mut Attributes {
        (**self).get_attribute_source_mut()
    }

    fn set_reader(&mut self, input: ReaderEnum) -> Result<()> {
        (**self).set_reader(input)
    }

    fn set_reader_test_point(&mut self) {
        (**self).set_reader_test_point()
    }
}
