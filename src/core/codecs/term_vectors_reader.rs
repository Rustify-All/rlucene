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
use crate::core::codecs::DefaultTermVectorsFormat;
use crate::core::codecs::term_vectors_format::TermVectorsFormat;
use crate::core::index::fields::{Fields, FieldsEnum2};
use crate::core::index::term_vectors::{RawTermVectors, TermVectors};
use crate::core::index::terms::TermsEnum2;
use crate::core::store::dummy::dummy_index_input::DummyIndexInput;
use crate::core::util::error::lucene_error::{LuceneError, Result};
/// Codec API for reading term vectors:
pub trait TermVectorsReader: TermVectors + Clone {
    /// Checks consistency of this reader.
    ///
    /// Note that this may be costly in terms of I/O, e.g. may involve computing
    /// a checksum value against large data files.
    fn check_integrity(&self) -> Result<()>;

    /// Returns an instance optimized for merging.
    ///
    /// This instance may only be used from the thread that acquires it.
    fn get_merge_instance(&self) -> Result<Option<Self>>
    where
        Self: Sized,
    {
        Ok(None)
    }
}
pub type DefaultTermVectorsReader<I> =
    <DefaultTermVectorsFormat as TermVectorsFormat>::TermVectorsReader<I>;

macro_rules! either_term_vectors_reader {
    ($vis:vis $name:ident => { fe: $fe:ident, te: $te:ident } { $( $Variant:ident : $T:ident ),+ $(,)? }) => {
        $vis enum $name<$( $T ),+> {
            $( $Variant($T), )+
        }

        impl<$( $T ),+> TermVectors for $name<$( $T ),+>
        where
            $( $T: TermVectorsReader ),+
        {
            type Fields = $fe<$( <$T as TermVectors>::Fields ),+>;

            type Terms = $te<$( <<$T as TermVectors>::Fields as Fields>::Terms ),+>;

            fn prefetch(&mut self, doc_id: i32) -> Result<()> {
                match self {
                    $( Self::$Variant(inner) => inner.prefetch(doc_id), )+
                }
            }

            fn get(&mut self, doc: i32) -> Result<Option<Self::Fields>> {
                match self {
                    $(
                        Self::$Variant(inner) => {
                            let fields = inner.get(doc)?;
                            Ok(fields.map($fe::$Variant))
                        }
                    ),+
                }
            }

            fn get_field_terms(
                &mut self,
                doc: i32,
                field: &str,
            ) -> Result<Option<<Self::Fields as Fields>::Terms>> {
                match self {
                    $(
                        Self::$Variant(inner) => {
                            let terms = inner.get_field_terms(doc, field)?;
                            Ok(terms.map($te::$Variant))
                        }
                    ),+
                }
            }
        }

        impl<$( $T ),+> Clone for $name<$( $T ),+>
        where
            $( $T: TermVectorsReader ),+
        {
            fn clone(&self) -> Self {
                match self {
                    $( Self::$Variant(inner) => Self::$Variant(inner.clone()), )+
                }
            }
        }

        impl<$( $T ),+> TermVectorsReader for $name<$( $T ),+>
        where
            $( $T: TermVectorsReader ),+
        {
            fn check_integrity(&self) -> Result<()> {
                match self {
                    $( Self::$Variant(inner) => inner.check_integrity(), )+
                }
            }

            fn get_merge_instance(&self) -> Result<Option<Self>>
            where
                Self: Sized,
            {
                match self {
                    $( Self::$Variant(inner) => match inner.get_merge_instance()? {
                        Some(value) => Ok(Some(Self::$Variant(value))),
                        None => Ok(None),
                    }, )+
                }
            }
        }
    };
}

either_term_vectors_reader!(
    pub TermVectorsReaderEnum2 => { fe: FieldsEnum2, te: TermsEnum2 } { A: A, B: B }
);

impl<A, B> RawTermVectors for TermVectorsReaderEnum2<A, B> {
    type IndexInput = DummyIndexInput;

    fn raw_term_vectors_mut(&mut self) -> Result<&mut DefaultTermVectorsReader<Self::IndexInput>> {
        Err(LuceneError::illegal_state(
            "raw term vectors reader is not available".to_string(),
        ))
    }

    fn raw_term_vectors(&self) -> Result<&DefaultTermVectorsReader<Self::IndexInput>> {
        Err(LuceneError::illegal_state(
            "raw term vectors reader is not available".to_string(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use crate::core::document::document::Document;
    use crate::core::document::field::Field;
    use crate::core::document::field_type::FieldType;
    use crate::core::document::stored_field::stored_field_type;
    use crate::core::document::text_field::text_field_type;
    use crate::core::util::error::lucene_error::{LuceneError, Result};
    use crate::test::index::random_index_writer::RandomIndexWriter;
    use crate::test::util::lucene_test_case::lucene_test_case_util::{
        new_directory_shared, random,
    };

    #[allow(dead_code)] // for quick search
    struct TestTermVectorsReader;

    #[test]
    fn test() -> Result<()> {
        // TODO
        Ok(())
    }
    #[test]
    fn test_reader() -> Result<()> {
        // TODO
        Ok(())
    }
    #[test]
    fn test_docs_enum() -> Result<()> {
        // TODO
        Ok(())
    }

    #[test]
    fn test_position_reader() -> Result<()> {
        // TODO
        Ok(())
    }
    #[test]
    fn test_offset_reader() -> Result<()> {
        // TODO
        Ok(())
    }
    #[test]
    fn test_illegal_payloads_without_positions() -> Result<()> {
        let mut random = random();

        let dir = new_directory_shared(&mut random)?;

        // TODO c需要使用带分词器的构造方法
        let w = RandomIndexWriter::new(&mut random, dir.clone());

        let mut ft = FieldType::from_ref(&*text_field_type::TYPE_NOT_STORED)?;
        ft.set_store_term_vectors(true)?;
        ft.set_store_term_vector_payloads(true)?;

        let mut doc = Document::new();
        doc.add(Field::new("field", "value", ft));

        let err = w.add_document(doc).unwrap_err();
        match err {
            LuceneError::IllegalArgument(msg) => {
                assert_eq!(
                    msg.to_string(),
                    "cannot index term vector payloads without term vector positions (field=\"field\")"
                );
            },
            _ => unreachable!("{:?}", err),
        }

        w.close()?;
        Ok(())
    }
    #[test]
    fn test_illegal_offsets_without_vectors() -> Result<()> {
        let mut random = random();

        let dir = new_directory_shared(&mut random)?;

        // TODO: 这里应该使用带分词器的构造方法
        let w = RandomIndexWriter::new(&mut random, dir.clone());

        let mut ft = FieldType::from_ref(&*text_field_type::TYPE_NOT_STORED)?;
        ft.set_store_term_vectors(false)?;
        ft.set_store_term_vector_offsets(true)?;

        let mut doc = Document::new();
        doc.add(Field::new("field", "value", ft));

        let err = w.add_document(doc).unwrap_err();
        match err {
            LuceneError::IllegalArgument(msg) => {
                assert_eq!(
                    msg.to_string(),
                    "cannot index term vector offsets when term vectors are not indexed (field=\"field\")"
                );
            },
            _ => unreachable!("{:?}", err),
        }

        w.close()?;
        Ok(())
    }
    #[test]
    fn test_illegal_positions_without_vectors() -> Result<()> {
        let mut random = random();

        let dir = new_directory_shared(&mut random)?;

        // TODO: 需要使用带分词器的构造方法
        let w = RandomIndexWriter::new(&mut random, dir.clone());

        let mut ft = FieldType::from_ref(&*text_field_type::TYPE_NOT_STORED)?;
        ft.set_store_term_vectors(false)?;
        ft.set_store_term_vector_positions(true)?;

        let mut doc = Document::new();
        doc.add(Field::new("field", "value", ft));

        let err = w.add_document(doc).unwrap_err();
        match err {
            LuceneError::IllegalArgument(msg) => {
                assert_eq!(
                    msg.to_string(),
                    "cannot index term vector positions when term vectors are not indexed (field=\"field\")"
                );
            },
            _ => unreachable!("{:?}", err),
        }

        w.close()?;
        Ok(())
    }
    #[test]
    fn test_illegal_vector_payloads_without_vectors() -> Result<()> {
        let mut random = random();
        let dir = new_directory_shared(&mut random)?;
        // TODO: 这里应该使用带分词器的构造方法
        let w = RandomIndexWriter::new(&mut random, dir.clone());

        let mut ft = FieldType::from_ref(&*text_field_type::TYPE_NOT_STORED)?;
        ft.set_store_term_vectors(false)?;
        ft.set_store_term_vector_payloads(true)?;

        let mut doc = Document::new();
        doc.add(Field::new("field", "value", ft));

        let err = w.add_document(doc).unwrap_err();
        match err {
            LuceneError::IllegalArgument(msg) => {
                assert_eq!(
                    msg.to_string(),
                    "cannot index term vector payloads when term vectors are not indexed (field=\"field\")"
                );
            },
            _ => unreachable!("{err:?}"),
        }

        w.close()?;
        Ok(())
    }

    #[test]
    fn test_illegal_vectors_without_indexed() -> Result<()> {
        let mut random = random();
        let dir = new_directory_shared(&mut random)?;
        // TODO: 这里应该使用带分词器的构造方法
        let w = RandomIndexWriter::new(&mut random, dir.clone());

        let mut ft = FieldType::from_ref(&*stored_field_type::TYPE)?;
        ft.set_store_term_vectors(true)?;

        let mut doc = Document::new();
        doc.add(Field::new("field", "value", ft));

        let err = w.add_document(doc).unwrap_err();
        match err {
            LuceneError::IllegalArgument(msg) => {
                assert_eq!(
                    msg.to_string(),
                    "cannot store term vectors for a field that is not indexed (field=\"field\")"
                );
            },
            _ => unreachable!("{err:?}"),
        }

        w.close()?;
        Ok(())
    }

    #[test]
    fn test_illegal_vector_positions_without_indexed() -> Result<()> {
        let mut random = random();
        let dir = new_directory_shared(&mut random)?;
        // TODO: 这里应该使用带分词器的构造方法
        let w = RandomIndexWriter::new(&mut random, dir.clone());

        let mut ft = FieldType::from_ref(&*stored_field_type::TYPE)?;
        ft.set_store_term_vector_positions(true)?;

        let mut doc = Document::new();
        doc.add(Field::new("field", "value", ft));

        let err = w.add_document(doc).unwrap_err();
        match err {
            LuceneError::IllegalArgument(msg) => {
                assert_eq!(
                    msg.to_string(),
                    "cannot store term vector positions for a field that is not indexed (field=\"field\")"
                );
            },
            _ => unreachable!("{err:?}"),
        }

        w.close()?;
        Ok(())
    }

    #[test]
    fn test_illegal_vector_offsets_without_indexed() -> Result<()> {
        let mut random = random();
        let dir = new_directory_shared(&mut random)?;
        // TODO: 这里应该使用带分词器的构造方法
        let w = RandomIndexWriter::new(&mut random, dir.clone());

        let mut ft = FieldType::from_ref(&*stored_field_type::TYPE)?;
        ft.set_store_term_vector_offsets(true)?;

        let mut doc = Document::new();
        doc.add(Field::new("field", "value", ft));

        let err = w.add_document(doc).unwrap_err();
        match err {
            LuceneError::IllegalArgument(msg) => {
                assert_eq!(
                    msg.to_string(),
                    "cannot store term vector offsets for a field that is not indexed (field=\"field\")"
                );
            },
            _ => unreachable!("{err:?}"),
        }

        w.close()?;
        Ok(())
    }

    #[test]
    fn test_illegal_vector_payloads_without_indexed() -> Result<()> {
        let mut random = random();
        let dir = new_directory_shared(&mut random)?;
        // TODO: 这里应该使用带分词器的构造方法
        let w = RandomIndexWriter::new(&mut random, dir.clone());

        let mut ft = FieldType::from_ref(&*stored_field_type::TYPE)?;
        ft.set_store_term_vector_payloads(true)?;

        let mut doc = Document::new();
        doc.add(Field::new("field", "value", ft));

        let err = w.add_document(doc).unwrap_err();
        match err {
            LuceneError::IllegalArgument(msg) => {
                assert_eq!(
                    msg.to_string(),
                    "cannot store term vector payloads for a field that is not indexed (field=\"field\")"
                );
            },
            _ => unreachable!("{err:?}"),
        }

        w.close()?;
        Ok(())
    }
}
