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
use crate::core::codecs::block_tree::lucene90_block_tree_terms_writer::Lucene90BlockTreeTermsWriter;
use crate::core::codecs::lucene101::lucene101_postings_writer::Lucene101PostingsWriter;
use crate::core::codecs::norms_producer::NormsProducer;
use crate::core::codecs::push_postings_writer_base::PushPostingsWriterBase;
use crate::core::index::fields::Fields;
use crate::core::store::IndexOutput;
use crate::core::util::error::lucene_error::Result;
/// Abstract API that consumes terms, doc, freq, prox, offset and payloads postings. Concrete
/// implementations of this actually do "something" with the postings (write it into the index in a
/// specific format).
pub trait FieldsConsumer {
    /// Write all fields, terms and postings. This is the "pull" API, allowing you to iterate more than
    /// once over the postings, somewhat analogous to using a DOM API to traverse an XML tree.
    ///
    /// # Notes
    ///
    /// - You must compute index statistics, including each Term’s `doc_freq` and `total_term_freq`, as
    ///   well as the summary `sum_total_term_freq`, `sum_total_doc_freq` and `doc_count`.
    /// - You must skip terms that have no docs and fields that have no terms, even though the
    ///   provided `Fields` API will expose them; this typically requires lazily writing the field or
    ///   term until you’ve actually seen the first term or document.
    /// - The provided `Fields` instance is limited: you cannot call any methods that return
    ///   statistics/counts; you cannot pass a non-null live docs when pulling docs/positions enums.
    fn write<F, N>(&mut self, fields: &mut F, norms: &Option<N>) -> Result<()>
    where
        F: Fields,
        N: NormsProducer;
    fn close(&mut self) -> Result<()>;
}

pub enum FieldsConsumerEnum<O>
where
    O: IndexOutput,
{
    Lucene90(Lucene90BlockTreeTermsWriter<O, PushPostingsWriterBase<Lucene101PostingsWriter<O>>>),
}
impl<O> FieldsConsumer for FieldsConsumerEnum<O>
where
    O: IndexOutput,
{
    fn write<F, N>(&mut self, fields: &mut F, norms: &Option<N>) -> Result<()>
    where
        F: Fields,
        N: NormsProducer,
    {
        match self {
            FieldsConsumerEnum::Lucene90(writer) => writer.write(fields, norms),
        }
    }

    fn close(&mut self) -> Result<()> {
        match self {
            FieldsConsumerEnum::Lucene90(writer) => writer.close(),
        }
    }
}
