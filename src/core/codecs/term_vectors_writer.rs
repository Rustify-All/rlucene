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
use crate::core::codecs::compressing::lucene90_compressing_term_vectors_writer::Lucene90CompressingTermVectorsWriter;
use crate::core::index::field_info::FieldInfo;
use crate::core::index::{BytesRef, BytesRefBuilder};
use crate::core::store::DataInput;
use crate::core::store::directory::Directory;
use crate::core::util::accountable::Accountable;
use crate::core::util::error::lucene_error::Result;

pub trait TermVectorsWriter: Accountable {
    fn start_document(&mut self, num_vector_fields: i32) -> Result<()>;

    fn finish_document(&mut self) -> Result<()> {
        Ok(())
    }
    fn start_field(
        &mut self,
        field_info: &FieldInfo,
        num_terms: usize,
        positions: bool,
        offsets: bool,
        payloads: bool,
    ) -> Result<()>;

    fn finish_field(&mut self) -> Result<()> {
        Ok(())
    }

    fn start_term(&mut self, term: &BytesRef<Vec<u8>>, freq: i32) -> Result<()>;

    fn finish_term(&mut self) -> Result<()> {
        Ok(())
    }

    fn add_position(
        &mut self,
        position: i32,
        start_offset: i32,
        end_offset: i32,
        payload: Option<&BytesRef<Vec<u8>>>,
    ) -> Result<()>;

    fn finish<D>(&mut self, num_docs: i32, dir: &D) -> Result<()>
    where
        D: Directory;

    fn add_prox(
        &mut self,
        num_prox: usize,
        positions: &mut Option<&mut impl DataInput>,
        offsets: &mut Option<&mut impl DataInput>,
    ) -> Result<()>;

    fn default_add_prox(
        &mut self,
        num_prox: usize,
        positions: &mut Option<&mut impl DataInput>,
        offsets: &mut Option<&mut impl DataInput>,
    ) -> Result<()> {
        let mut position = 0;
        let mut last_offset = 0;
        let mut payload: Option<BytesRefBuilder<Vec<u8>>> = None;

        for _ in 0..num_prox {
            let this_payload = if let Some(pos_input) = positions.as_mut() {
                let code = pos_input.read_vint()?;
                position += (code as u32 >> 1) as i32;

                if code & 1 != 0 {
                    let payload_len = pos_input.read_vint()? as usize;

                    if payload.is_none() {
                        payload = Some(BytesRefBuilder::new());
                    }
                    let builder = payload.as_mut().unwrap();
                    builder.grow_no_copy(payload_len);
                    pos_input.read_bytes(&mut builder.bytes_ref.bytes, 0, payload_len as i32)?;
                    builder.set_length(payload_len);
                    Some(builder.get_bytes_ref())
                } else {
                    None
                }
            } else {
                position = -1;
                None
            };

            // --- offsets ---
            let (start_offset, end_offset) = if let Some(off_input) = offsets.as_mut() {
                let start = last_offset + off_input.read_vint()?;
                let end = start + off_input.read_vint()?;
                last_offset = end;
                (start, end)
            } else {
                (-1, -1)
            };

            self.add_position(position, start_offset, end_offset, this_payload)?;
        }

        Ok(())
    }
}

pub enum TermVectorsWriterEnum<D>
where
    D: Directory,
{
    Lucene90(Lucene90CompressingTermVectorsWriter<D>),
}

impl<D> Accountable for TermVectorsWriterEnum<D>
where
    D: Directory,
{
    fn ram_bytes_used(&self) -> Result<i64> {
        match self {
            TermVectorsWriterEnum::Lucene90(writer) => writer.ram_bytes_used(),
        }
    }
}

impl<D> TermVectorsWriter for TermVectorsWriterEnum<D>
where
    D: Directory,
{
    fn start_document(&mut self, num_vector_fields: i32) -> Result<()> {
        match self {
            TermVectorsWriterEnum::Lucene90(writer) => writer.start_document(num_vector_fields),
        }
    }

    fn finish_document(&mut self) -> Result<()> {
        match self {
            TermVectorsWriterEnum::Lucene90(writer) => writer.finish_document(),
        }
    }

    fn start_field(
        &mut self,
        field_info: &FieldInfo,
        num_terms: usize,
        positions: bool,
        offsets: bool,
        payloads: bool,
    ) -> Result<()> {
        match self {
            TermVectorsWriterEnum::Lucene90(writer) => {
                writer.start_field(field_info, num_terms, positions, offsets, payloads)
            },
        }
    }

    fn finish_field(&mut self) -> Result<()> {
        match self {
            TermVectorsWriterEnum::Lucene90(writer) => writer.finish_field(),
        }
    }

    fn start_term(&mut self, term: &BytesRef<Vec<u8>>, freq: i32) -> Result<()> {
        match self {
            TermVectorsWriterEnum::Lucene90(writer) => writer.start_term(term, freq),
        }
    }

    fn finish_term(&mut self) -> Result<()> {
        match self {
            TermVectorsWriterEnum::Lucene90(writer) => writer.finish_term(),
        }
    }

    fn add_position(
        &mut self,
        position: i32,
        start_offset: i32,
        end_offset: i32,
        payload: Option<&BytesRef<Vec<u8>>>,
    ) -> Result<()> {
        match self {
            TermVectorsWriterEnum::Lucene90(writer) => {
                writer.add_position(position, start_offset, end_offset, payload)
            },
        }
    }

    fn finish<D1>(&mut self, num_docs: i32, dir: &D1) -> Result<()>
    where
        D1: Directory,
    {
        match self {
            TermVectorsWriterEnum::Lucene90(writer) => writer.finish(num_docs, dir),
        }
    }

    fn add_prox(
        &mut self,
        num_prox: usize,
        positions: &mut Option<&mut impl DataInput>,
        offsets: &mut Option<&mut impl DataInput>,
    ) -> Result<()> {
        match self {
            TermVectorsWriterEnum::Lucene90(writer) => {
                writer.add_prox(num_prox, positions, offsets)
            },
        }
    }

    fn default_add_prox(
        &mut self,
        num_prox: usize,
        positions: &mut Option<&mut impl DataInput>,
        offsets: &mut Option<&mut impl DataInput>,
    ) -> Result<()> {
        match self {
            TermVectorsWriterEnum::Lucene90(writer) => {
                writer.default_add_prox(num_prox, positions, offsets)
            },
        }
    }
}
