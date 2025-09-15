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
use crate::analysis::common::analysis_impl::core::whitespace_analyzer::WhitespaceAnalyzerTS;
use crate::core::analysis::dummy::dummy_token_stream::DummyTokenStream;
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
pub struct TokenStreamBase {
    pub(crate) att: Attributes,
}
impl TokenStreamBase {
    pub fn new(att: Attributes) -> Self {
        Self { att }
    }
}

pub fn default_attribute() -> Attributes {
    Attributes::PackedToken(PackedTokenAttributeImpl::new())
}
macro_rules! either_token_stream {
    ($vis:vis $name:ident { $( $Variant:ident : $T:ident ),+ $(,)? }) => {
        $vis enum $name<$( $T ),+> {
            $( $Variant($T), )+
        }

        impl<$( $T ),+> TokenStream for $name<$( $T ),+>
        where
            $( $T: TokenStream ),+
        {
            #[inline]
            fn increment_token(&mut self) -> Result<bool> {
                match self { $( Self::$Variant(inner) => inner.increment_token(), )+ }
            }

            #[inline]
            fn end(&mut self) -> Result<()> {
                match self { $( Self::$Variant(inner) => inner.end(), )+ }
            }

            #[inline]
            fn default_end(&mut self) -> Result<()> {
                match self { $( Self::$Variant(inner) => TokenStream::default_end(inner), )+ }
            }

            #[inline]
            fn reset(&mut self) -> Result<()> {
                match self { $( Self::$Variant(inner) => inner.reset(), )+ }
            }

            #[inline]
            fn default_reset(&mut self) -> Result<()> {
                match self { $( Self::$Variant(inner) => TokenStream::default_reset(inner), )+ }
            }

            #[inline]
            fn close(&mut self) -> Result<()> {
                match self { $( Self::$Variant(inner) => inner.close(), )+ }
            }

            #[inline]
            fn get_attribute_source(&self) -> &Attributes {
                match self { $( Self::$Variant(inner) => inner.get_attribute_source(), )+ }
            }

            #[inline]
            fn get_attribute_source_mut(&mut self) -> &mut Attributes {
                match self { $( Self::$Variant(inner) => inner.get_attribute_source_mut(), )+ }
            }

            #[inline]
            fn set_reader(&mut self, input: ReaderEnum) -> Result<()> {
                match self { $( Self::$Variant(inner) => inner.set_reader(input), )+ }
            }

            #[inline]
            fn set_reader_test_point(&mut self) {
                match self { $( Self::$Variant(inner) => inner.set_reader_test_point(), )+ }
            }
        }
    };
}
either_token_stream!(pub EitherTokenStream { Whitespace: A, Dummy: B });
either_token_stream!(pub Either2TokenStream { A: A, B: B });

pub type InnerTokenStreams = EitherTokenStream<WhitespaceAnalyzerTS, DummyTokenStream>;

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
