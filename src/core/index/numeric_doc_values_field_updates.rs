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
use parking_lot::Mutex;
use std::borrow::Cow;
use std::sync::Arc;

use crate::core::index::BytesRef;
use crate::core::index::doc_values_field_updates::{
    AbstractIterator, AbstractIteratorBase, DocValuesFieldInnerIter, DocValuesFieldIterator,
    DocValuesFieldIteratorEnum, DocValuesFieldUpdatesBase, PAGE_SIZE,
    SingleValueDocValuesFieldUpdatesBase,
};
use crate::core::index::doc_values_type::DocValuesType;
use crate::core::util::accountable::Accountable;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::long_values::LongValues;
use crate::core::util::packed::PackedInts;
use crate::core::util::packed::abstract_paged_mutable::{
    AbstractPagedMutable, AbstractPagedMutableBaseEnum,
};
use crate::core::util::packed::paged_growable_writer::PagedGrowableWriter;
use crate::core::util::packed::paged_mutable::PagedMutable;

/// A `DocValuesFieldUpdates` which holds updates of documents, of a single `NumericDocValuesField`.
pub(crate) struct NumericDocValuesFieldUpdates {
    values: AbstractPagedMutable<AbstractPagedMutableBaseEnum>,
    min_value: i64,
    lock: Mutex<()>,

    values_iter: Option<Arc<AbstractPagedMutable<AbstractPagedMutableBaseEnum>>>,
}
impl NumericDocValuesFieldUpdates {
    pub(crate) fn new() -> Result<NumericDocValuesFieldUpdates> {
        let sub_reader = AbstractPagedMutableBaseEnum::GrowableWriter(
            PagedGrowableWriter::with_fill_page(1, PackedInts::DEFAULT),
        );
        let values = AbstractPagedMutable::new(1, PAGE_SIZE, sub_reader)?;
        Ok(NumericDocValuesFieldUpdates {
            values,
            min_value: 0,
            lock: Mutex::new(()),
            values_iter: None,
        })
    }
    pub(crate) fn with_range(
        min_value: i64,
        max_value: i64,
    ) -> Result<NumericDocValuesFieldUpdates> {
        let bits_per_value = PackedInts::unsigned_bits_required(max_value - min_value);
        let sub_reader = AbstractPagedMutableBaseEnum::Mutable(PagedMutable::with_overhead_ratio(
            PAGE_SIZE,
            bits_per_value,
            PackedInts::DEFAULT,
        ));
        let values = AbstractPagedMutable::new(1, PAGE_SIZE, sub_reader)?;
        Ok(NumericDocValuesFieldUpdates {
            values,
            min_value,
            lock: Mutex::new(()),
            values_iter: None,
        })
    }
}

impl DocValuesFieldUpdatesBase for NumericDocValuesFieldUpdates {
    fn finish(&mut self) {
        self.values_iter = Some(Arc::new(std::mem::take(&mut self.values)));
    }

    fn add_value(&mut self, _doc: i32, value: i64, index: i32) -> Result<()> {
        let _guard = self.lock.lock();
        self.values.set(index as i64, value - self.min_value);
        Ok(())
    }

    fn add_byte_ref(&mut self, _doc: i32, _value: &BytesRef<Vec<u8>>, _index: i32) -> Result<()> {
        Err(LuceneError::unreachable(
            "numericDocValuesFieldUpdates does not support add_byte_ref",
        ))
    }

    fn add_iterator<I: DocValuesFieldIterator>(
        &mut self,
        doc_id: i32,
        iterator: &mut I,
    ) -> Result<()> {
        self.add_value(doc_id, iterator.long_value()?, 0)
    }

    fn iterator(
        &self,
        inner: DocValuesFieldInnerIter,
        del_gen: i64,
    ) -> Result<DocValuesFieldIteratorEnum> {
        debug_assert!(self.values_iter.is_some());
        let base = AbstractIteratorNumeric::new(
            self.values_iter.as_ref().unwrap().clone(),
            0,
            self.min_value,
        );
        Ok(DocValuesFieldIteratorEnum::AbstractNumeric(
            AbstractIterator::new(inner, del_gen, base),
        ))
    }

    fn swap(&mut self, i: i32, j: i32) -> Result<()> {
        let tmp_val = self.values.get_mut(j as i64)?;
        let value = self.values.get_mut(i as i64)?;
        self.values.set(j as i64, value);
        self.values.set(i as i64, tmp_val);
        Ok(())
    }

    fn grow(&mut self, size: i32) -> Result<()> {
        let value_result = self.values.grow_with_size(size as i64)?;
        if let Some(values) = value_result {
            self.values = values;
        }
        Ok(())
    }

    fn resize(&mut self, size: i32) -> Result<()> {
        self.values = self.values.resize(size as i64)?;
        Ok(())
    }

    fn sub_type(&self) -> DocValuesType {
        DocValuesType::Numeric
    }
}

impl Accountable for NumericDocValuesFieldUpdates {
    fn ram_bytes_used(&self) -> Result<i64> {
        todo!()
    }
}
pub(crate) struct AbstractIteratorNumeric {
    values: Arc<AbstractPagedMutable<AbstractPagedMutableBaseEnum>>,
    value: i64,
    min_value: i64,
}
impl AbstractIteratorNumeric {
    pub(crate) fn new(
        values: Arc<AbstractPagedMutable<AbstractPagedMutableBaseEnum>>,
        value: i64,
        min_value: i64,
    ) -> Self {
        AbstractIteratorNumeric {
            values,
            value,
            min_value,
        }
    }
}
impl AbstractIteratorBase for AbstractIteratorNumeric {
    fn set(&mut self, idx: i64) -> Result<()> {
        self.value = self.values.get(idx)? + self.min_value;
        Ok(())
    }

    fn long_value(&self) -> Result<i64> {
        Ok(self.value)
    }

    fn binary_value(&mut self) -> Result<Cow<'_, BytesRef<Vec<u8>>>> {
        unreachable!("NumericDocValuesFieldUpdatesIterator does not support binary_value")
    }
}

#[derive(Default)]
pub struct SingleValueNumericDocValuesFieldUpdates {
    value: i64,
}
impl SingleValueNumericDocValuesFieldUpdates {
    pub(crate) fn new(value: i64) -> SingleValueNumericDocValuesFieldUpdates {
        SingleValueNumericDocValuesFieldUpdates { value }
    }
}
impl SingleValueDocValuesFieldUpdatesBase for SingleValueNumericDocValuesFieldUpdates {
    fn binary_value(&self) -> Result<Cow<'_, BytesRef<Vec<u8>>>> {
        Err(LuceneError::unreachable(
            "SingleValueNumericDocValuesFieldUpdates does not support binary_value",
        ))
    }

    fn long_value(&self) -> Result<i64> {
        Ok(self.value)
    }

    fn sub_type(&self) -> DocValuesType {
        DocValuesType::Numeric
    }
}

#[cfg(test)]
mod tests {
    use crate::core::document::binary_doc_values_field::BinaryDocValuesField;
    use crate::core::document::document::Document;
    use crate::core::document::field::Store;
    use crate::core::document::numeric_doc_values_field::NumericDocValuesField;
    use crate::core::document::sorted_doc_values_field::SortedDocValuesField;
    use crate::core::document::sorted_set_doc_values_field::SortedSetDocValuesField;
    use crate::core::document::string_field::StringField;
    use crate::core::index::binary_doc_values::BinaryDocValues;
    use crate::core::index::composite_reader::get_context;
    use crate::core::index::directory_reader::directory_reader_util;
    use crate::core::index::field_infos::FieldInfos;
    use crate::core::index::index_reader::IndexReader;
    use crate::core::index::index_reader_context::IndexReaderContext;
    use crate::core::index::index_writer::IndexWriter;
    use crate::core::index::index_writer_config::{DEFAULT_RAM_BUFFER_SIZE_MB, DISABLE_AUTO_FLUSH};
    use crate::core::index::leaf_reader::LeafReader;
    use crate::core::index::live_index_writer_config::LiveIndexWriterConfig;
    use crate::core::index::numeric_doc_values::NumericDocValues;
    use crate::core::index::sort::Sort;
    use crate::core::index::sorted_doc_values::SortedDocValues;
    use crate::core::index::sorted_set_doc_values::SortedSetDocValues;
    use crate::core::index::term::Term;
    use crate::core::search::doc_id_set_iterator::DocIdSetIterator;
    use crate::core::search::index_searcher::IndexSearcher;
    use crate::core::search::sort_field::{SortField, SortFieldType};
    use crate::core::search::term_query::TermQuery;
    use crate::core::search::top_docs::TopDocsLike;
    use crate::core::store::directory::Directory;
    use crate::core::util::bits::Bits;
    use crate::core::util::error::lucene_error::{LuceneError, Result};
    use crate::test::util::lucene_test_case::lucene_test_case_util::{
        at_least, is_night_mode, new_bytes_ref_from_string, new_directory, new_index_writer_config,
        random,
    };
    use crate::test::util::test_util::TestUtil;
    use rand::Rng;
    use rand::seq::IndexedRandom;
    use std::collections::HashSet;
    use std::sync::Arc;
    use std::vec;

    #[allow(dead_code)]
    struct TestNumericDocValuesUpdates;
    fn doc(id: i32) -> Result<Document> {
        // make sure we don't set the doc's value to 0, to not confuse with a document that's missing values
        doc_with_val(id, (id + 1) as i64)
    }

    fn doc_with_val(id: i32, val: i64) -> Result<Document> {
        let mut doc = Document::new();
        doc.add(StringField::with_string(
            "id",
            format!("doc-{}", id),
            Store::No,
        )?);
        doc.add(NumericDocValuesField::new("val", val));
        Ok(doc)
    }
    #[test]
    fn test_multiple_updates_same_doc() -> Result<()> {
        let mut random = random();
        let dir = Arc::new(new_directory(&mut random)?);
        // TODO: 未实现MockAnalyzer
        let mut config = new_index_writer_config(&mut random);
        config.set_max_buffered_docs(3); // small number of docs
        let writer = IndexWriter::new(dir.clone(), config)?;

        writer.update_documents_with_term(
            Term::from_text("id", "doc-1"),
            doc_with_val(1, 1_000_000_000)?,
        )?;
        writer.update_numeric_doc_value(Term::from_text("id", "doc-1"), "val", 1_000_001_111)?;
        writer.update_documents_with_term(
            Term::from_text("id", "doc-2"),
            doc_with_val(2, 2_000_000_000)?,
        )?;
        writer.update_documents_with_term(
            Term::from_text("id", "doc-2"),
            doc_with_val(2, 2_222_222_222)?,
        )?;
        writer.update_numeric_doc_value(Term::from_text("id", "doc-1"), "val", 1_111_111_111)?;

        let reader = if random.random_bool(0.5) {
            writer.commit()?;
            directory_reader_util::open(dir.clone())?
        } else {
            directory_reader_util::open_with_writer(&writer)?
        };
        let reader = get_context(Arc::new(reader))?;
        let mut searcher = IndexSearcher::new(reader)?;

        let td = searcher.search_with_sort(
            TermQuery::new(Term::from_text("id", "doc-1")),
            1,
            Sort::with_fields(vec![
                SortField::new(Some("val"), SortFieldType::Long)?.into(),
            ])?,
        )?;
        assert_eq!(td.score_docs().len(), 1, "doc-1 missing?");
        assert_eq!(
            *td.base.score_docs[0].fields()?[0].as_i64().unwrap(),
            1_111_111_111,
            "doc-1 value mismatch"
        );

        let td = searcher.search_with_sort(
            TermQuery::new(Term::from_text("id", "doc-2")),
            1,
            Sort::with_fields(vec![
                SortField::new(Some("val"), SortFieldType::Long)?.into(),
            ])?,
        )?;
        assert_eq!(td.score_docs().len(), 1, "doc-2 missing?");
        assert_eq!(
            *td.base.score_docs[0].fields()?[0].as_i64().unwrap(),
            2_222_222_222,
            "doc-2 value mismatch"
        );

        writer.close()?;
        Ok(())
    }
    // TODO: 测试未通过
    fn test_biased_mix_of_random_updates() -> Result<()> {
        // 3 types of operations: add, updated, updateDV.
        // rather then randomizing equally, we'll pick (random) cutoffs so each test run is biased,
        // in terms of some ops happen more often then others
        let mut random = random();
        let dir = Arc::new(new_directory(&mut random)?);
        // TODO: 未实现 MockAnalyzer
        let config = new_index_writer_config(&mut random);
        let writer = IndexWriter::new(dir.clone(), config)?;

        let add_cutoff = TestUtil::next_int(&mut random, 1, 98);
        let upd_cutoff = TestUtil::next_int(&mut random, add_cutoff + 1, 99);

        let num_operations = at_least(&mut random, 1000);
        let mut expected: std::collections::HashMap<i32, i64> =
            std::collections::HashMap::with_capacity((num_operations / 3) as usize);

        // start with at least one doc before any chance of updates
        let num_seed_docs = at_least(&mut random, 1);
        for i in 0..num_seed_docs {
            let val = random.random();
            expected.insert(i, val);
            writer.add_document(doc_with_val(i, val)?)?;
        }

        for _ in 0..num_operations {
            let op = TestUtil::next_int(&mut random, 1, 100);
            let val = random.random();
            if op <= add_cutoff {
                let id = expected.len() as i32;
                expected.insert(id, val);
                writer.add_document(doc_with_val(id, val)?)?;
            } else {
                let id = TestUtil::next_int(&mut random, 0, expected.len() as i32 - 1);
                expected.insert(id, val);
                if op <= upd_cutoff {
                    writer.update_documents_with_term(
                        Term::from_text("id", &format!("doc-{id}")),
                        doc_with_val(id, val)?,
                    )?;
                } else {
                    writer.update_numeric_doc_value(
                        Term::from_text("id", &format!("doc-{id}")),
                        "val",
                        val,
                    )?;
                }
            }
        }

        writer.commit()?;

        let reader = directory_reader_util::open(dir.clone())?;
        let reader = get_context(Arc::new(reader))?;
        let mut searcher = IndexSearcher::new(reader)?;

        for (id, expected_val) in expected {
            let td = searcher.search_with_sort(
                TermQuery::new(Term::from_text("id", &format!("doc-{}", id))),
                1,
                Sort::with_fields(vec![
                    SortField::new(Some("val"), SortFieldType::Long)?.into(),
                ])?,
            )?;
            assert_eq!(td.total_hits().value, 1, "{}", format!("{} missing?", id));
            assert_eq!(
                *td.base.score_docs[0].fields()?[0].as_i64().unwrap(),
                expected_val,
                "{}",
                format!("{} value", id)
            );
        }

        Ok(())
    }

    #[test]
    fn test_updates_are_flushed() -> Result<()> {
        let mut random = random();
        let dir = Arc::new(new_directory(&mut random)?);
        // TODO: 未实现 MockAnalyzer
        let mut config = new_index_writer_config(&mut random);
        config.set_ram_buffer_size_mb(0.00000001);
        let mut writer = IndexWriter::new(dir.clone(), config)?;

        writer.add_document(doc(0)?)?; // val=1
        writer.add_document(doc(1)?)?; // val=2
        writer.add_document(doc(3)?)?; // val=4
        writer.commit()?;

        assert_eq!(1, writer.get_flush_deletes_count());

        writer.update_numeric_doc_value(Term::from_text("id", "doc-0"), "val", 5)?;
        assert_eq!(2, writer.get_flush_deletes_count());

        writer.update_numeric_doc_value(Term::from_text("id", "doc-1"), "val", 6)?;
        assert_eq!(3, writer.get_flush_deletes_count());

        writer.update_numeric_doc_value(Term::from_text("id", "doc-2"), "val", 7)?;
        assert_eq!(4, writer.get_flush_deletes_count());

        writer.get_config_mut().set_ram_buffer_size_mb(1000.0);
        writer.update_numeric_doc_value(Term::from_text("id", "doc-2"), "val", 7)?;
        assert_eq!(4, writer.get_flush_deletes_count());

        writer.close()?;
        Ok(())
    }

    #[test]
    fn test_simple() -> Result<()> {
        let mut random = random();
        let dir = Arc::new(new_directory(&mut random)?);
        // TODO: 未实现 MockAnalyzer
        let mut config = new_index_writer_config(&mut random);
        // make sure random config doesn't flush on us
        config.set_max_buffered_docs(10);
        config.set_ram_buffer_size_mb(DISABLE_AUTO_FLUSH as f64);
        let writer = IndexWriter::new(dir.clone(), config)?;

        writer.add_document(doc(0)?)?; // val=1
        writer.add_document(doc(1)?)?; // val=2
        if random.random_bool(0.5) {
            // randomly commit before the update is sent
            writer.commit()?;
        }

        writer.update_numeric_doc_value(Term::from_text("id", "doc-0"), "val", 2)?;

        let reader = if random.random_bool(0.5) {
            writer.close()?;
            directory_reader_util::open(dir.clone())?
        } else {
            let r = directory_reader_util::open_with_writer(&writer)?;
            writer.close()?;
            r
        };

        let reader = get_context(Arc::new(reader))?;
        assert_eq!(reader.leaves()?.len(), 1);
        let r = reader.leaves()?[0].reader();
        let mut ndv = r.get_numeric_doc_values("val")?.unwrap();
        assert_eq!(ndv.next_doc()?, 0);
        assert_eq!(ndv.long_value()?, 2);
        assert_eq!(ndv.next_doc()?, 1);
        assert_eq!(ndv.long_value()?, 2);

        Ok(())
    }
    #[test]
    fn test_update_few_segments() -> Result<()> {
        let mut random = random();
        let dir = Arc::new(new_directory(&mut random)?);
        // TODO: 未实现 MockAnalyzer / NoMergePolicy
        let mut config = new_index_writer_config(&mut random);
        config.set_max_buffered_docs(2); // generate few segments
        let writer = IndexWriter::new(dir.clone(), config)?;

        let num_docs = 10;
        let mut expected_values = vec![0i64; num_docs];
        for i in 0..num_docs {
            writer.add_document(doc(i as i32)?)?;
            expected_values[i] = (i + 1) as i64;
        }
        writer.commit()?;

        // update few docs
        for i in 0..num_docs {
            if random.random_range(0.0..1.0) < 0.4 {
                let value = ((i + 1) * 2) as i64;
                writer.update_numeric_doc_value(
                    Term::from_text("id", &format!("doc-{}", i)),
                    "val",
                    value,
                )?;
                expected_values[i] = value;
            }
        }

        let reader = if random.random_bool(0.5) {
            writer.close()?;
            directory_reader_util::open(dir.clone())?
        } else {
            let r = directory_reader_util::open_with_writer(&writer)?;
            writer.close()?;
            r
        };
        let reader = get_context(Arc::new(reader))?;

        for context in reader.leaves()?.iter() {
            let r = context.reader();
            let mut ndv = r.get_numeric_doc_values("val")?.unwrap();
            for i in 0..r.max_doc()? {
                let expected = expected_values[(i + context.doc_base) as usize];
                assert_eq!(i, ndv.next_doc()?);
                let actual = ndv.long_value()?;
                assert_eq!(expected, actual);
            }
        }

        Ok(())
    }
    #[test]
    fn test_reopen() -> Result<()> {
        // TODO
        Ok(())
    }
    #[test]
    fn test_updates_and_deletes() -> Result<()> {
        // TODO
        Ok(())
    }
    #[test]
    fn test_updates_with_deletes() -> Result<()> {
        // update and delete different documents in the same commit session
        let mut random = random();
        let dir = Arc::new(new_directory(&mut random)?);
        // TODO: 未实现 MockAnalyzer / NoMergePolicy
        let mut config = new_index_writer_config(&mut random);
        config.set_max_buffered_docs(10);
        let writer = IndexWriter::new(dir.clone(), config)?;

        writer.add_document(doc(0)?)?;
        writer.add_document(doc(1)?)?;

        if random.random_bool(0.5) {
            writer.commit()?;
        }

        writer.delete_documents_with_terms(vec![Term::from_text("id", "doc-0")])?;
        writer.update_numeric_doc_value(Term::from_text("id", "doc-1"), "val", 17)?;

        let reader = if random.random_bool(0.5) {
            writer.close()?;
            directory_reader_util::open(dir.clone())?
        } else {
            let r = directory_reader_util::open_with_writer(&writer)?;
            writer.close()?;
            r
        };

        let reader = get_context(Arc::new(reader))?;
        let leaf = &reader.leaves()?[0];
        let r = leaf.reader();
        let live_docs = r.get_live_docs()?.unwrap();
        assert!(!live_docs.get(0));
        let mut ndv = r.get_numeric_doc_values("val")?.unwrap();
        assert_eq!(ndv.advance(1)?, 1);
        assert_eq!(ndv.long_value()?, 17);

        Ok(())
    }
    // TODO: 测试未通过
    fn test_multiple_doc_values_types() -> Result<()> {
        let mut random = random();
        let dir = Arc::new(new_directory(&mut random)?);
        // TODO: 未实现 MockAnalyzer
        let mut config = new_index_writer_config(&mut random);
        config.set_max_buffered_docs(10); // prevent merges
        let writer = IndexWriter::new(dir.clone(), config)?;

        for i in 0..4 {
            let mut doc = Document::new();
            doc.add(StringField::with_string("dvUpdateKey", "dv", Store::No)?);
            doc.add(NumericDocValuesField::new("ndv", i as i64));
            doc.add(BinaryDocValuesField::new(
                "bdv",
                new_bytes_ref_from_string(&mut random, &i.to_string())?,
            ));
            doc.add(SortedDocValuesField::new(
                "sdv",
                new_bytes_ref_from_string(&mut random, &i.to_string())?,
            ));
            doc.add(SortedSetDocValuesField::new(
                "ssdv",
                new_bytes_ref_from_string(&mut random, &i.to_string())?,
            ));
            doc.add(SortedSetDocValuesField::new(
                "ssdv",
                new_bytes_ref_from_string(&mut random, &(i * 2).to_string())?,
            ));
            writer.add_document(doc)?;
        }
        writer.commit()?;

        // update all docs' ndv field
        writer.update_numeric_doc_value(Term::from_text("dvUpdateKey", "dv"), "ndv", 17)?;
        writer.close()?;

        let reader = directory_reader_util::open(dir.clone())?;
        let leaf = get_context(Arc::new(reader))?;
        let r = leaf.leaves()?[0].reader();

        let mut ndv = r.get_numeric_doc_values("ndv")?.unwrap();
        let mut bdv = r.get_binary_doc_values("bdv")?.unwrap();
        let mut sdv = r.get_sorted_doc_values("sdv")?.unwrap();
        let mut ssdv = r.get_sorted_set_doc_values("ssdv")?.unwrap();

        for i in 0..r.max_doc()? {
            // numeric
            assert_eq!(i, ndv.next_doc()?);
            assert_eq!(17, ndv.long_value()?);

            // binary
            assert_eq!(i, bdv.next_doc()?);
            let term = bdv.binary_value()?.utf8_to_string()?;
            assert_eq!(term, i.to_string());

            // sorted
            assert_eq!(i, sdv.next_doc()?);
            let ord_value = sdv.ord_value()?;
            let term = sdv.lookup_ord(ord_value)?.utf8_to_string()?;
            assert_eq!(term, i.to_string());

            // sorted set
            assert_eq!(i, ssdv.next_doc()?);
            let ord = ssdv.next_ord()?;
            let term = ssdv.lookup_ord(ord)?.utf8_to_string()?;
            assert_eq!(i, term.parse::<i32>()?);

            if i == 0 {
                assert_eq!(1, ssdv.doc_value_count()?);
            } else {
                assert_eq!(2, ssdv.doc_value_count()?);
                let ord = ssdv.next_ord()?;
                let term = ssdv.lookup_ord(ord)?.utf8_to_string()?;
                assert_eq!(i * 2, term.parse::<i32>()?);
            }
        }
        Ok(())
    }
    #[test]
    fn test_multiple_numeric_doc_values() -> Result<()> {
        let mut random = random();
        let dir = Arc::new(new_directory(&mut random)?);
        // TODO: 未实现 MockAnalyzer
        let mut config = new_index_writer_config(&mut random);
        config.set_max_buffered_docs(10); // prevent merges
        let writer = IndexWriter::new(dir.clone(), config)?;

        for i in 0..2 {
            let mut doc = Document::new();
            doc.add(StringField::with_string("dvUpdateKey", "dv", Store::No)?);
            doc.add(NumericDocValuesField::new("ndv1", i as i64));
            doc.add(NumericDocValuesField::new("ndv2", i as i64));
            writer.add_document(doc)?;
        }
        writer.commit()?;

        // update all docs' ndv1 field
        writer.update_numeric_doc_value(Term::from_text("dvUpdateKey", "dv"), "ndv1", 17)?;
        writer.close()?;

        let reader = directory_reader_util::open(dir.clone())?;
        let reader = get_context(Arc::new(reader))?;
        let r = reader.leaves()?[0].reader();

        let mut ndv1 = r.get_numeric_doc_values("ndv1")?.unwrap();
        let mut ndv2 = r.get_numeric_doc_values("ndv2")?.unwrap();

        for i in 0..r.max_doc()? {
            assert_eq!(i, ndv1.next_doc()?);
            assert_eq!(17, ndv1.long_value()?);

            assert_eq!(i, ndv2.next_doc()?);
            assert_eq!(i as i64, ndv2.long_value()?);
        }
        Ok(())
    }
    #[test]
    fn test_document_with_no_value() -> Result<()> {
        let mut random = random();
        let dir = Arc::new(new_directory(&mut random)?);
        // TODO: 未实现 MockAnalyzer
        let config = new_index_writer_config(&mut random);
        let writer = IndexWriter::new(dir.clone(), config)?;

        for i in 0..2 {
            let mut doc = Document::new();
            doc.add(StringField::with_string("dvUpdateKey", "dv", Store::No)?);
            if i == 0 {
                // index only one document with value
                doc.add(NumericDocValuesField::new("ndv", 5));
            }
            writer.add_document(doc)?;
        }
        writer.commit()?;

        // update all docs' ndv field
        writer.update_numeric_doc_value(Term::from_text("dvUpdateKey", "dv"), "ndv", 17)?;
        writer.close()?;

        let reader = directory_reader_util::open(dir.clone())?;
        let reader = get_context(Arc::new(reader))?;
        let r = reader.leaves()?[0].reader();

        let mut ndv = r.get_numeric_doc_values("ndv")?.unwrap();
        for i in 0..r.max_doc()? {
            assert_eq!(i, ndv.next_doc()?);
            assert_eq!(
                17,
                ndv.long_value()?,
                "doc={} has wrong numeric doc value",
                i
            );
        }

        Ok(())
    }
    #[test]
    fn test_update_non_numeric_doc_values_field() -> Result<()> {
        // we don't support adding new fields or updating existing non-numeric-dv fields through numeric updates
        let mut random = random();
        let dir = Arc::new(new_directory(&mut random)?);
        // TODO: 未实现 MockAnalyzer
        let config = new_index_writer_config(&mut random);
        let writer = IndexWriter::new(dir.clone(), config)?;

        let mut doc = Document::new();
        doc.add(StringField::with_string("key", "doc", Store::No)?);
        doc.add(StringField::with_string("foo", "bar", Store::No)?);
        writer.add_document(doc)?;
        writer.commit()?;
        let mut doc = Document::new();
        doc.add(StringField::with_string("key", "doc", Store::No)?);
        doc.add(StringField::with_string("foo", "bar", Store::No)?);
        writer.add_document(doc)?;

        let res = writer.update_numeric_doc_value(Term::from_text("key", "doc"), "ndv", 17);
        assert!(matches!(res, Err(LuceneError::IllegalArgument(_))));

        // attempt to update a non-numeric field "foo"
        let res = writer.update_numeric_doc_value(Term::from_text("key", "doc"), "foo", 17);
        assert!(matches!(res, Err(LuceneError::IllegalArgument(_))));

        writer.close()?;
        Ok(())
    }
    #[test]
    fn test_different_dv_format_per_field() -> Result<()> {
        // TODO
        Ok(())
    }

    #[test]
    fn test_update_same_doc_multiple_times() -> Result<()> {
        // TODO
        Ok(())
    }
    #[test]
    fn test_segment_merges() -> Result<()> {
        // TODO
        Ok(())
    }
    #[test]
    fn test_update_document_by_multiple_terms() -> Result<()> {
        // TODO
        Ok(())
    }
    #[test]
    fn test_sorted_index() -> Result<()> {
        // TODO
        Ok(())
    }
    #[test]
    fn test_many_reopens_and_fields() -> Result<()> {
        // TODO
        Ok(())
    }
    #[test]
    fn test_update_segment_with_no_doc_values() -> Result<()> {
        let mut random = random();
        let dir = Arc::new(new_directory(&mut random)?);
        // TODO: 未实现 MockAnalyzer 与 NoMergePolicy
        let config = new_index_writer_config(&mut random);
        let writer = IndexWriter::new(dir.clone(), config)?;

        // first segment with NDV
        let mut doc = Document::new();
        doc.add(StringField::with_string("id", "doc0", Store::No)?);
        doc.add(NumericDocValuesField::new("ndv", 3));
        writer.add_document(doc)?;

        let mut doc = Document::new();
        doc.add(StringField::with_string("id", "doc4", Store::No)?); // document without 'ndv' field
        writer.add_document(doc)?;
        writer.commit()?;

        // second segment with no NDV
        let mut doc = Document::new();
        doc.add(StringField::with_string("id", "doc1", Store::No)?);
        writer.add_document(doc)?;

        let mut doc = Document::new();
        doc.add(StringField::with_string("id", "doc2", Store::No)?); // document that isn't updated
        writer.add_document(doc)?;
        writer.commit()?;

        // update document in the first segment - should not affect docsWithField of
        // the document without NDV field
        writer.update_numeric_doc_value(Term::from_text("id", "doc0"), "ndv", 5)?;
        // update document in the second segment - field should be added and we should
        // be able to handle the other document correctly (e.g. no NPE)
        writer.update_numeric_doc_value(Term::from_text("id", "doc1"), "ndv", 5)?;
        writer.close()?;

        let reader = directory_reader_util::open(dir.clone())?;
        let reader = get_context(Arc::new(reader))?;
        for ctx in reader.leaves()? {
            let r = ctx.reader();
            let mut ndv = r.get_numeric_doc_values("ndv")?.unwrap();
            assert_eq!(ndv.next_doc()?, 0);
            assert_eq!(ndv.long_value()?, 5);
            // docID 1 has no ndv value
            assert!(ndv.next_doc()? > 1);
        }

        Ok(())
    }
    #[test]
    fn test_update_segment_with_no_doc_values2() -> Result<()> {
        // TODO
        Ok(())
    }
    #[test]
    fn test_update_segment_with_posting_but_no_doc_values() -> Result<()> {
        let mut random = random();
        let dir = Arc::new(new_directory(&mut random)?);
        // TODO: 未实现 MockAnalyzer / NoMergePolicy
        let config = new_index_writer_config(&mut random);
        let writer = IndexWriter::new(dir.clone(), config)?;

        // first segment with ndv and ndv2 fields
        let mut doc = Document::new();
        doc.add(StringField::with_string("id", "doc0", Store::No)?);
        doc.add(NumericDocValuesField::new("ndv", 5));
        doc.add(StringField::with_string("ndv2", "10", Store::No)?);
        doc.add(NumericDocValuesField::new("ndv2", 10));
        writer.add_document(doc)?;
        writer.commit()?;

        // second segment with no ndv and ndv2 fields
        let mut doc = Document::new();
        doc.add(StringField::with_string("id", "doc1", Store::No)?);
        writer.add_document(doc)?;
        writer.commit()?;

        // update docValues of "ndv" field in the second segment (allowed)
        writer.update_numeric_doc_value(Term::from_text("id", "doc1"), "ndv", 5)?;

        // update docValues of "ndv2" field in the second segment (NOT allowed)
        let result = writer.update_numeric_doc_value(Term::from_text("id", "doc1"), "ndv2", 10);
        assert!(matches!(result, Err(LuceneError::IllegalArgument(_))));
        let actual_err_msg = "Can't update [Numeric] doc values; the field [ndv2] must be doc values only field, but is also indexed with postings.";
        assert_eq!(actual_err_msg, result.unwrap_err().to_string());

        writer.close()?;

        // Verify index content
        let reader = directory_reader_util::open(dir.clone())?;
        let reader = get_context(Arc::new(reader))?;
        for ctx in reader.leaves()? {
            let r = ctx.reader();
            let mut ndv = r.get_numeric_doc_values("ndv")?.unwrap();
            for i in 0..r.max_doc()? {
                assert_eq!(i, ndv.next_doc()?);
                assert_eq!(5, ndv.long_value()?);
            }
        }

        Ok(())
    }
    #[test]
    fn test_update_numeric_dv_field_with_same_name_as_posting_field() -> Result<()> {
        // this used to fail because FieldInfos::Builder neglected to update globalFieldMaps.docValuesTypes map
        let mut random = random();
        let dir = Arc::new(new_directory(&mut random)?);
        // TODO: 未实现 MockAnalyzer
        let config = new_index_writer_config(&mut random);
        let writer = IndexWriter::new(dir.clone(), config)?;

        // add document with both posting field and NDV field of the same name
        let mut doc = Document::new();
        doc.add(StringField::with_string("f", "mock-value", Store::No)?);
        doc.add(NumericDocValuesField::new("f", 5));
        writer.add_document(doc)?;
        writer.commit()?;

        let result = writer.update_numeric_doc_value(Term::from_text("f", "mock-value"), "f", 17);
        assert!(matches!(result, Err(LuceneError::IllegalArgument(_))));
        let actual_err_msg = "Can't update [Numeric] doc values; the field [f] must be doc values only field, but is also indexed with postings.";
        assert_eq!(actual_err_msg, result.unwrap_err().to_string());

        writer.close()?;

        // verify NDV content unchanged
        let reader = directory_reader_util::open(dir.clone())?;
        let reader = get_context(Arc::new(reader))?;
        let mut ndv = reader.leaves()?[0]
            .reader()
            .get_numeric_doc_values("f")?
            .unwrap();
        assert_eq!(ndv.next_doc()?, 0);
        assert_eq!(ndv.long_value()?, 5);

        Ok(())
    }
    #[test]
    fn test_stress_multi_threading() -> Result<()> {
        // TODO
        Ok(())
    }
    #[test]
    fn test_update_different_docs_in_different_gens() -> Result<()> {
        // update same document multiple times across generations
        let mut random = random();
        let dir = Arc::new(new_directory(&mut random)?);
        // TODO: 未实现 MockAnalyzer
        let mut config = new_index_writer_config(&mut random);
        config.set_max_buffered_docs(4);
        let writer = IndexWriter::new(dir.clone(), config)?;

        let num_docs = at_least(&mut random, 10);
        for i in 0..num_docs {
            let mut doc = Document::new();
            doc.add(StringField::with_string(
                "id",
                format!("doc{}", i),
                Store::No,
            )?);
            let value = random.random();
            doc.add(NumericDocValuesField::new("f", value));
            doc.add(NumericDocValuesField::new("cf", value * 2));
            writer.add_document(doc)?;
        }

        let num_gens = at_least(&mut random, 5);
        for _ in 0..num_gens {
            let doc_id = random.random_range(0..num_docs);
            let t = Term::from_text("id", &format!("doc{}", doc_id));
            let value = random.random();
            writer.update_doc_values(
                t,
                vec![
                    NumericDocValuesField::new("f", value).into(),
                    NumericDocValuesField::new("cf", value * 2).into(),
                ],
            )?;

            let reader = directory_reader_util::open_with_writer(&writer)?;
            let reader = get_context(Arc::new(reader))?;
            for ctx in reader.leaves()? {
                let r = ctx.reader();
                let mut fndv = r.get_numeric_doc_values("f")?.unwrap();
                let mut cfndv = r.get_numeric_doc_values("cf")?.unwrap();

                for j in 0..r.max_doc()? {
                    assert_eq!(j, fndv.next_doc()?);
                    assert_eq!(j, cfndv.next_doc()?);
                    assert_eq!(cfndv.long_value()?, fndv.long_value()? * 2);
                }
            }
        }

        writer.close()?;
        Ok(())
    }

    #[test]
    fn test_change_codec() -> Result<()> {
        // this test is not required in Rust Lucene
        Ok(())
    }
    #[test]
    fn test_add_indexes() -> Result<()> {
        // TODO
        Ok(())
    }
    #[test]
    fn test_add_new_field_after_add_indexes() -> Result<()> {
        // TODO
        Ok(())
    }
    #[test]
    fn test_updates_after_add_indexes() -> Result<()> {
        // TODO
        Ok(())
    }
    fn ensure_consistent_field_infos(old: &FieldInfos, after: &FieldInfos) -> Result<()> {
        for fi in old.iter() {
            let by_number = after.field_info_by_number(fi.number)?;
            assert!(by_number.is_some());

            let by_name = after.field_info_by_name(&fi.name);
            assert!(by_name.is_some());

            let after_fi = by_name.unwrap();
            assert_eq!(fi.number, after_fi.number,);
            assert!(fi.get_doc_values_gen() <= after_fi.get_doc_values_gen(),);
        }
        Ok(())
    }
    #[test]
    fn test_delete_unused_updates_files() -> Result<()> {
        let mut random = random();
        let dir = Arc::new(new_directory(&mut random)?);
        // TODO: 未实现 MockAnalyzer
        let config = new_index_writer_config(&mut random);
        let writer = IndexWriter::new(dir.clone(), config)?;

        let mut doc = Document::new();
        doc.add(StringField::with_string("id", "d0", Store::No)?);
        doc.add(NumericDocValuesField::new("f1", 1));
        doc.add(NumericDocValuesField::new("f2", 1));
        writer.add_document(doc)?;

        // update each field twice to make sure all unneeded files are deleted
        for f in ["f1", "f2"] {
            writer.update_numeric_doc_value(Term::from_text("id", "d0"), f, 2)?;
            writer.commit()?;
            let num_files = dir.list_all()?.len();

            // update again, number of files shouldn't change (old field's gen is
            // removed)
            writer.update_numeric_doc_value(Term::from_text("id", "d0"), f, 3)?;
            writer.commit()?;

            assert_eq!(num_files, dir.list_all()?.len(),);
        }

        writer.close()?;
        Ok(())
    }
    // TODO: 测试未通过
    fn test_tons_of_updates() -> Result<()> {
        // LUCENE-5248: make sure that when there are many updates, we don't use too much RAM
        let mut random = random();
        let dir = Arc::new(new_directory(&mut random)?);

        let mut config = new_index_writer_config(&mut random);
        config.set_ram_buffer_size_mb(DEFAULT_RAM_BUFFER_SIZE_MB);
        config.set_max_buffered_docs(DISABLE_AUTO_FLUSH);
        let mut writer = IndexWriter::new(dir.clone(), config)?;

        // test data: lots of documents (few hundred to few 10Ks) and lots of update terms
        let num_docs = if is_night_mode() {
            at_least(&mut random, 20_000)
        } else {
            at_least(&mut random, 200)
        };
        let num_numeric_fields = at_least(&mut random, 5);
        let num_terms = random.random_range(10..=100); // terms should affect many docs

        let mut update_terms = HashSet::new();
        while update_terms.len() < num_terms as usize {
            update_terms.insert(TestUtil::random_simple_string(&mut random));
        }
        let update_terms: Vec<_> = update_terms.into_iter().collect();

        // build a large index with many NDV fields and update terms
        for _ in 0..num_docs {
            let mut doc = Document::new();
            let num_update_terms = random.random_range(1..=(num_terms / 10).max(1));
            for _ in 0..num_update_terms {
                let term_val = update_terms.choose(&mut random).unwrap();
                doc.add(StringField::with_string("upd", term_val, Store::No)?);
            }
            for j in 0..num_numeric_fields {
                let val = random.random();
                doc.add(NumericDocValuesField::new(format!("f{}", j), val));
                doc.add(NumericDocValuesField::new(format!("cf{}", j), val * 2));
            }
            writer.add_document(doc)?;
        }

        writer.commit()?; // commit so there's something to apply to

        // set to flush every 2048 bytes (approximately every 12 updates), so we get
        // many flushes during numeric updates
        writer
            .get_config_mut()
            .set_ram_buffer_size_mb(2048.0 / 1024.0 / 1024.0);
        let num_updates = at_least(&mut random, 100);

        for _ in 0..num_updates {
            let field = random.random_range(0..num_numeric_fields);
            let term_val = update_terms.choose(&mut random).unwrap();
            let update_term = Term::from_text("upd", term_val);
            let value = random.random();
            writer.update_doc_values(
                update_term,
                vec![
                    NumericDocValuesField::new(format!("f{}", field), value).into(),
                    NumericDocValuesField::new(format!("cf{}", field), value * 2).into(),
                ],
            )?;
        }

        writer.close()?;

        // validate
        let reader = directory_reader_util::open(dir.clone())?;
        let reader = get_context(Arc::new(reader))?;
        for ctx in reader.leaves()? {
            let r = ctx.reader();
            for i in 0..num_numeric_fields {
                let mut f = r.get_numeric_doc_values(&format!("f{}", i))?.unwrap();
                let mut cf = r.get_numeric_doc_values(&format!("cf{}", i))?.unwrap();
                for j in 0..r.max_doc()? {
                    assert_eq!(j, f.next_doc()?);
                    assert_eq!(j, cf.next_doc()?);
                    assert_eq!(
                        cf.long_value()?,
                        f.long_value()? * 2,
                        "reader={}, field=f{}, doc={}",
                        r,
                        i,
                        j
                    );
                }
            }
        }

        Ok(())
    }
    #[test]
    fn test_updates_order() -> Result<()> {
        let mut random = random();
        let dir = Arc::new(new_directory(&mut random)?);
        // TODO: 未实现 MockAnalyzer
        let config = new_index_writer_config(&mut random);
        let writer = IndexWriter::new(dir.clone(), config)?;

        // add initial document
        let mut doc = Document::new();
        doc.add(StringField::with_string("upd", "t1", Store::No)?);
        doc.add(StringField::with_string("upd", "t2", Store::No)?);
        doc.add(NumericDocValuesField::new("f1", 1));
        doc.add(NumericDocValuesField::new("f2", 1));
        writer.add_document(doc)?;

        // apply updates in specific order
        writer.update_numeric_doc_value(Term::from_text("upd", "t1"), "f1", 2)?;
        writer.update_numeric_doc_value(Term::from_text("upd", "t1"), "f2", 2)?;
        writer.update_numeric_doc_value(Term::from_text("upd", "t2"), "f1", 3)?;
        writer.update_numeric_doc_value(Term::from_text("upd", "t2"), "f2", 3)?;
        writer.update_numeric_doc_value(Term::from_text("upd", "t1"), "f1", 4)?;
        writer.close()?;

        // verify the latest values
        let reader = directory_reader_util::open(dir.clone())?;
        let reader = get_context(Arc::new(reader))?;
        let r = reader.leaves()?[0].reader();

        let mut f1 = r.get_numeric_doc_values("f1")?.unwrap();
        assert_eq!(f1.next_doc()?, 0);
        assert_eq!(f1.long_value()?, 4);

        let mut f2 = r.get_numeric_doc_values("f2")?.unwrap();
        assert_eq!(f2.next_doc()?, 0);
        assert_eq!(f2.long_value()?, 3);

        Ok(())
    }
    #[test]
    fn test_update_all_deleted_segment() -> Result<()> {
        let mut random = random();
        let dir = Arc::new(new_directory(&mut random)?);
        // TODO: 未实现 MockAnalyzer
        let config = new_index_writer_config(&mut random);
        let writer = IndexWriter::new(dir.clone(), config)?;

        // add and commit documents
        let mut doc = Document::new();
        doc.add(StringField::with_string("id", "doc", Store::No)?);
        doc.add(NumericDocValuesField::new("f1", 1));
        writer.add_document(doc)?;
        let mut doc = Document::new();
        doc.add(StringField::with_string("id", "doc", Store::No)?);
        doc.add(NumericDocValuesField::new("f1", 1));
        writer.add_document(doc)?;
        writer.commit()?;

        writer.delete_documents_with_terms(vec![Term::from_text("id", "doc")])?;

        let mut doc = Document::new();
        doc.add(StringField::with_string("id", "doc", Store::No)?);
        doc.add(NumericDocValuesField::new("f1", 1));
        writer.add_document(doc)?;
        writer.update_numeric_doc_value(Term::from_text("id", "doc"), "f1", 2)?;
        writer.close()?;

        // verify only one segment remains and update was applied
        let reader = directory_reader_util::open(dir.clone())?;
        let reader = get_context(Arc::new(reader))?;
        assert_eq!(reader.leaves()?.len(), 1);

        let r = reader.leaves()?[0].reader();
        let mut dvs = r.get_numeric_doc_values("f1")?.unwrap();
        assert_eq!(dvs.next_doc()?, 0);
        assert_eq!(dvs.long_value()?, 2);

        Ok(())
    }
    #[test]
    fn test_update_two_nonexisting_terms() -> Result<()> {
        let mut random = random();
        let dir = Arc::new(new_directory(&mut random)?);
        // TODO: 未实现 MockAnalyzer
        let config = new_index_writer_config(&mut random);
        let writer = IndexWriter::new(dir.clone(), config)?;

        // add a document
        let mut doc = Document::new();
        doc.add(StringField::with_string("id", "doc", Store::No)?);
        doc.add(NumericDocValuesField::new("f1", 1));
        writer.add_document(doc)?;

        // update with multiple nonexisting terms in the same field
        writer.update_numeric_doc_value(Term::from_text("c", "foo"), "f1", 2)?;
        writer.update_numeric_doc_value(Term::from_text("c", "bar"), "f1", 2)?;
        writer.close()?;

        // verify the value remains unchanged
        let reader = directory_reader_util::open(dir.clone())?;
        let reader = get_context(Arc::new(reader))?;
        assert_eq!(reader.leaves()?.len(), 1);

        let r = reader.leaves()?[0].reader();
        let mut dvs = r.get_numeric_doc_values("f1")?.unwrap();
        assert_eq!(dvs.next_doc()?, 0);
        assert_eq!(dvs.long_value()?, 1);

        Ok(())
    }
    #[test]
    fn test_io_context() -> Result<()> {
        // TODO
        Ok(())
    }
}
