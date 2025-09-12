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
use crate::core::codecs::compressing::lucene90_compressing_stored_fields_writer::Lucene90CompressingStoredFieldsWriter;
use crate::core::codecs::stored_fields_reader::StoredFieldsReader;
use crate::core::index::field_info::FieldInfo;
use crate::core::index::field_infos::FieldInfos;
use crate::core::index::merge_state::{DocMapEnum, MergeState};
use crate::core::index::stored_field_visitor::{Status, StoredFieldVisitor};
use crate::core::index::stored_fields::StoredFields;
use crate::core::index::{BytesRef, DocIDMerger, Sub, SubBase, of};
use crate::core::search::doc_id_set_iterator::disi_const::NO_MORE_DOCS;
use crate::core::store::directory::Directory;
use crate::core::store::{DataInput, IndexInput};
use crate::core::util::accountable::Accountable;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

/// Codec API for writing stored fields:
///
/// 1. For every document,
///    [`startDocument()`](StoredFieldsWriter::start_document) is called,
///    informing the Codec that a new document has started.
/// 2. `writeField` is called for each field in the document.
/// 3. After all documents have been written,
///    [`finish(int)`](StoredFieldsWriter::finish) is called for
///    verification/sanity-checks.
/// 4. Finally, the writer is closed.
pub trait StoredFieldsWriter {
    /// Called before writing the stored fields of the document.
    /// `write_field` will be called for each stored field.
    /// This is called even if the document has no stored fields.
    fn start_document(&mut self) -> Result<()>;

    /// Called when a document and all its fields have been added.
    fn finish_document(&mut self) -> Result<()> {
        Ok(())
    }

    /// Writes a stored int value.
    fn write_field_i32(&mut self, field_info: &FieldInfo, value: i32) -> Result<()>;

    /// Writes a stored long value.
    fn write_field_i64(&mut self, field_info: &FieldInfo, value: i64) -> Result<()>;

    /// Writes a stored float value.
    fn write_field_f32(&mut self, field_info: &FieldInfo, value: f32) -> Result<()>;

    /// Writes a stored double value.
    fn write_field_f64(&mut self, field_info: &FieldInfo, value: f64) -> Result<()>;

    /// Writes a stored binary value from a [`DataInput`] and a `length`.
    fn write_field_with_input(
        &mut self,
        field_info: &FieldInfo,
        input: &mut impl DataInput,
        length: i32,
    ) -> Result<()> {
        let mut buf = vec![0u8; length as usize];
        input.read_bytes(&mut buf, 0, length)?;
        self.write_field_bytes(field_info, &BytesRef::from_slice(buf, 0, length as usize))
    }

    /// Writes a stored binary value.
    fn write_field_bytes(
        &mut self,
        field_info: &FieldInfo,
        value: &BytesRef<Vec<u8>>,
    ) -> Result<()>;

    /// Writes a stored string value.
    fn write_field_str(&mut self, field_info: &FieldInfo, value: &str) -> Result<()>;

    /**
     * Called before `Drop`, passing in the number of documents that were
     * written. Note that this is intentionally redundant (equivalent to
     * the number of calls to
     * [`startDocument`](StoredFieldsWriter::start_document),
     * but a Codec should check that this is the case to detect the JRE bug
     * described in LUCENE-1282.
     */
    fn finish<D>(&mut self, num_docs: i32, dir: &D) -> Result<()>
    where
        D: Directory;
    /// Merges in the stored fields from the readers in `mergeState`. The
    /// default implementation skips over deleted documents, and uses
    /// [`startDocument()`](StoredFieldsWriter::start_document), `writeField`,
    /// and [`finish(int)`](StoredFieldsWriter::finish), returning the number of
    /// documents that were written. Implementations can override this
    /// method for more sophisticated merging (bulk-byte copying, etc.).
    fn merge<I, D>(&mut self, merge_state: &mut MergeState<I>, dir: &D) -> Result<i32>
    where
        I: IndexInput,
        D: Directory,
        Self: Sized,
    {
        let mut subs = Vec::with_capacity(merge_state.stored_fields_readers.len());

        for i in 0..merge_state.stored_fields_readers.len() {
            {
                let reader = &mut merge_state.stored_fields_readers[i];
                reader.check_integrity()?;
            }
            let visitor = MergeVisitor::new(merge_state, i)?;

            subs.push(Rc::new(RefCell::new(Sub::new(StoredFieldsMergeSub::new(
                visitor,
                Rc::clone(&merge_state.doc_maps[i]),
                i,
                merge_state.max_docs[i],
            )))));
        }

        let mut doc_count = 0;
        let mut doc_id_merger = of(subs, merge_state.needs_index_sort)?;

        while let Some(sub_rc) = doc_id_merger.next()? {
            let mut sub = sub_rc.borrow_mut();
            debug_assert_eq!(sub.mapped_doc_id, doc_count);

            self.start_document()?;
            let reader = &mut merge_state.stored_fields_readers[sub.sub.reader_index];
            reader.document_with_visitor(sub.sub.doc_id, &mut sub.sub.visitor, self)?;
            self.finish_document()?;
            doc_count += 1;
        }

        self.finish(doc_count, dir)?;
        Ok(doc_count)
    }
}
struct StoredFieldsMergeSub {
    pub reader_index: usize,
    pub max_doc: i32,
    pub visitor: MergeVisitor,
    pub doc_id: i32,
    pub doc_map: Rc<DocMapEnum>,
}

impl StoredFieldsMergeSub {
    fn new(
        visitor: MergeVisitor,
        doc_map: Rc<DocMapEnum>,
        reader_index: usize,
        max_doc: i32,
    ) -> Self {
        Self {
            reader_index,
            max_doc,
            visitor,
            doc_id: -1,
            doc_map,
        }
    }
    fn reader_index(&self) -> usize {
        self.reader_index
    }
}
impl SubBase for StoredFieldsMergeSub {
    fn next_doc(&mut self) -> Result<i32> {
        self.doc_id += 1;
        if self.doc_id == self.max_doc {
            Ok(NO_MORE_DOCS)
        } else {
            Ok(self.doc_id)
        }
    }

    fn get_doc_map(&self) -> Result<&Rc<DocMapEnum>> {
        todo!()
    }
}
// use for padding
impl Default for StoredFieldsMergeSub {
    fn default() -> Self {
        Self {
            reader_index: 0,
            max_doc: 0,
            visitor: MergeVisitor::default(),
            doc_id: -1,
            doc_map: Rc::new(DocMapEnum::default()),
        }
    }
}
/// A visitor that adds every field it sees.
#[derive(Default, Clone)]
pub(crate) struct MergeVisitor {
    remapper: Option<Rc<FieldInfos>>,
}
impl MergeVisitor {
    pub(crate) fn new<I>(merge_state: &MergeState<I>, reader_index: usize) -> Result<Self>
    where
        I: IndexInput,
    {
        for fi in merge_state.field_infos[reader_index].as_ref() {
            if let Some(other) = merge_state
                .merge_field_infos
                .field_info_by_number(fi.number)?
            {
                if other.name != fi.name {
                    return Ok(Self {
                        remapper: Some(Rc::clone(&merge_state.merge_field_infos)),
                    });
                }
            } else {
                return Ok(Self {
                    remapper: Some(Rc::clone(&merge_state.merge_field_infos)),
                });
            }
        }
        Ok(Self { remapper: None })
    }
    fn remap(&self, field: Arc<FieldInfo>) -> Result<Arc<FieldInfo>> {
        if let Some(ref remapper) = self.remapper {
            // field numbers are not aligned, we need to remap to the new field
            // number
            match remapper.field_info_by_name(&field.name) {
                Some(new_field) => Ok(new_field),
                None => Err(LuceneError::illegal_state(format!(
                    "FieldInfo not found in remapper with filed_name: {}",
                    field.name
                ))),
            }
        } else {
            Ok(field)
        }
    }
}
impl StoredFieldVisitor for MergeVisitor {
    fn binary_field_with_input(
        &mut self,
        field_info: Arc<FieldInfo>,
        input: &mut impl DataInput,
        length: i32,
        writer: &mut impl StoredFieldsWriter,
    ) -> Result<()> {
        writer.write_field_with_input(self.remap(field_info)?.as_ref(), input, length)
    }

    fn binary_field(
        &mut self,
        field_info: Arc<FieldInfo>,
        value: Vec<u8>,
        writer: &mut impl StoredFieldsWriter,
    ) -> Result<()> {
        writer.write_field_bytes(
            self.remap(field_info)?.as_ref(),
            &BytesRef::from_bytes(value),
        )
    }

    fn string_field(
        &mut self,
        field_info: Arc<FieldInfo>,
        value: String,
        writer: &mut impl StoredFieldsWriter,
    ) -> Result<()> {
        writer.write_field_str(self.remap(field_info)?.as_ref(), &value)
    }

    fn int_field(
        &mut self,
        field_info: Arc<FieldInfo>,
        value: i32,
        writer: &mut impl StoredFieldsWriter,
    ) -> Result<()> {
        writer.write_field_i32(self.remap(field_info)?.as_ref(), value)
    }

    fn long_field(
        &mut self,
        field_info: Arc<FieldInfo>,
        value: i64,
        writer: &mut impl StoredFieldsWriter,
    ) -> Result<()> {
        writer.write_field_i64(self.remap(field_info)?.as_ref(), value)
    }

    fn float_field(
        &mut self,
        field_info: Arc<FieldInfo>,
        value: f32,
        writer: &mut impl StoredFieldsWriter,
    ) -> Result<()> {
        writer.write_field_f32(self.remap(field_info)?.as_ref(), value)
    }

    fn double_field(
        &mut self,
        field_info: Arc<FieldInfo>,
        value: f64,
        writer: &mut impl StoredFieldsWriter,
    ) -> Result<()> {
        writer.write_field_f64(self.remap(field_info)?.as_ref(), value)
    }

    fn needs_field(
        &mut self,
        _field_info: Arc<FieldInfo>,
        _writer: &mut impl StoredFieldsWriter,
    ) -> Result<Status> {
        Ok(Status::Yes)
    }
}

pub enum StoredFieldsWriterEnum<D>
where
    D: Directory,
{
    Lucene90(Lucene90CompressingStoredFieldsWriter<D>),
}
impl<D> StoredFieldsWriter for StoredFieldsWriterEnum<D>
where
    D: Directory,
{
    fn start_document(&mut self) -> Result<()> {
        match self {
            StoredFieldsWriterEnum::Lucene90(writer) => writer.start_document(),
        }
    }

    fn finish_document(&mut self) -> Result<()> {
        match self {
            StoredFieldsWriterEnum::Lucene90(writer) => writer.finish_document(),
        }
    }

    fn write_field_i32(&mut self, field_info: &FieldInfo, value: i32) -> Result<()> {
        match self {
            StoredFieldsWriterEnum::Lucene90(writer) => writer.write_field_i32(field_info, value),
        }
    }

    fn write_field_i64(&mut self, field_info: &FieldInfo, value: i64) -> Result<()> {
        match self {
            StoredFieldsWriterEnum::Lucene90(writer) => writer.write_field_i64(field_info, value),
        }
    }

    fn write_field_f32(&mut self, field_info: &FieldInfo, value: f32) -> Result<()> {
        match self {
            StoredFieldsWriterEnum::Lucene90(writer) => writer.write_field_f32(field_info, value),
        }
    }

    fn write_field_f64(&mut self, field_info: &FieldInfo, value: f64) -> Result<()> {
        match self {
            StoredFieldsWriterEnum::Lucene90(writer) => writer.write_field_f64(field_info, value),
        }
    }

    fn write_field_with_input(
        &mut self,
        field_info: &FieldInfo,
        input: &mut impl DataInput,
        length: i32,
    ) -> Result<()> {
        match self {
            StoredFieldsWriterEnum::Lucene90(writer) => {
                writer.write_field_with_input(field_info, input, length)
            },
        }
    }

    fn write_field_bytes(
        &mut self,
        field_info: &FieldInfo,
        value: &BytesRef<Vec<u8>>,
    ) -> Result<()> {
        match self {
            StoredFieldsWriterEnum::Lucene90(writer) => writer.write_field_bytes(field_info, value),
        }
    }

    fn write_field_str(&mut self, field_info: &FieldInfo, value: &str) -> Result<()> {
        match self {
            StoredFieldsWriterEnum::Lucene90(writer) => writer.write_field_str(field_info, value),
        }
    }

    fn finish<D1>(&mut self, num_docs: i32, dir: &D1) -> Result<()>
    where
        D1: Directory,
    {
        match self {
            StoredFieldsWriterEnum::Lucene90(writer) => writer.finish(num_docs, dir),
        }
    }

    fn merge<I, D1>(&mut self, merge_state: &mut MergeState<I>, dir: &D1) -> Result<i32>
    where
        I: IndexInput,
        D1: Directory,
        Self: Sized,
    {
        match self {
            StoredFieldsWriterEnum::Lucene90(writer) => writer.merge(merge_state, dir),
        }
    }
}
impl<D> Accountable for StoredFieldsWriterEnum<D>
where
    D: Directory,
{
    fn ram_bytes_used(&self) -> Result<i64> {
        match self {
            StoredFieldsWriterEnum::Lucene90(writer) => writer.ram_bytes_used(),
        }
    }
}
