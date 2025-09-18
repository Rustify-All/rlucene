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
use rand::Rng;
use rand_xoshiro::Xoroshiro128Plus;
use rand_xoshiro::rand_core::SeedableRng;

use crate::core::store::DataInput;
use crate::core::store::data_output::DataOutput;
use crate::core::store::output_stream_data_output::OutputStreamDataOutput;
use crate::core::util::error::lucene_error::Result;

pub trait BaseDataOutputTestCase {
    type DO: DataOutput;

    fn new_instance(&self) -> Result<Self::DO>;
    fn get_bytes(&mut self, instance: Self::DO) -> Vec<u8>;

    fn test_randomized_writes<DI: DataInput, R: Rng + ?Sized>(
        &mut self,
        random: &mut R,
    ) -> Result<()> {
        let seed: u64 = random.random();
        let mut instance = self.new_instance()?;
        let mut buffer = Vec::new();
        let mut os = OutputStreamDataOutput::new(&mut buffer);
        let max = 500000;
        let mut random1 = Xoroshiro128Plus::seed_from_u64(seed);
        let mut random2 = Xoroshiro128Plus::seed_from_u64(seed);

        add_random_data(&mut instance, &mut random1, max);
        add_random_data(&mut os, &mut random2, max);
        assert_eq!(&self.get_bytes(instance), os.os.into_inner().unwrap());
        Ok(())
    }
}

pub enum DataInputAction {
    ReadByte(u8),
    ReadBytes(Vec<u8>),
    ReadBytesRange {
        bytes: Vec<u8>,
        off: usize,
        length: usize,
    },
    ReadInt(i32),
    ReadLong(i64),
    ReadShort(i16),
    ReadVInt(i32),
    ReadZInt(i32),
    ReadZLong(i64),
    ReadVLong(i64),
    ReadString(String),
}

impl DataInputAction {
    pub fn verify<DI: DataInput>(&self, src: &mut DI) {
        match self {
            DataInputAction::ReadByte(value) => {
                assert_eq!(src.read_byte().unwrap(), *value, "Condition failed for DI");
            },
            DataInputAction::ReadBytes(bytes) => {
                let mut buffer = vec![0u8; bytes.len()];
                let _ = src.read_bytes(&mut buffer, 0, bytes.len() as i32);
                assert_eq!(
                    buffer.as_slice(),
                    bytes.as_slice(),
                    "Condition failed for DI"
                );
            },
            DataInputAction::ReadBytesRange { bytes, off, length } => {
                let mut read: Vec<u8> = vec![0u8; bytes.len() + *off];
                let _ = src.read_bytes(&mut read, *off as i32, *length as i32);
                assert_eq!(
                    read[*off..*off + *length],
                    bytes[*off..*off + *length],
                    "readBytes(byte[], off)",
                );
            },
            DataInputAction::ReadInt(value) => {
                assert_eq!(src.read_int().unwrap(), *value, "readInt()");
            },
            DataInputAction::ReadLong(value) => {
                assert_eq!(src.read_long().unwrap(), *value, "readLong()");
            },
            DataInputAction::ReadShort(value) => {
                assert_eq!(src.read_short().unwrap(), *value, "readShort()");
            },
            DataInputAction::ReadVInt(value) => {
                assert_eq!(src.read_vint().unwrap(), *value, "readVInt()");
            },
            DataInputAction::ReadZInt(value) => {
                assert_eq!(src.read_zint().unwrap(), *value, "readZInt()");
            },
            DataInputAction::ReadZLong(value) => {
                assert_eq!(src.read_zlong().unwrap(), *value, "readZLong()");
            },
            DataInputAction::ReadVLong(value) => {
                assert_eq!(src.read_vlong().unwrap(), *value, "readVLong()");
            },
            DataInputAction::ReadString(value) => {
                assert_eq!(src.read_string().unwrap(), value.as_str(), "readString()");
            },
        }
    }
}

const GENERATOR_COUNT: usize = 11;

pub fn add_random_data(
    dst: &mut impl DataOutput,
    rnd: &mut impl Rng,
    max_add_calls: i32,
) -> Vec<DataInputAction> {
    let mut vec: Vec<DataInputAction> = Vec::new();
    for _i in 0..max_add_calls {
        let action = match rnd.random_range(0..GENERATOR_COUNT) {
            //0 writeByte / readByte
            0 => {
                let value: u8 = rnd.random();
                let _ = dst.write_byte(value);
                DataInputAction::ReadByte(value)
            },
            //1 writeBytes / readBytes (array and buffer version).
            1 => {
                let len = rnd.random_range(0..100);
                let bytes: Vec<u8> = (0..len).map(|_| rnd.random()).collect();
                let bytes_len = bytes.len();
                let _ = dst.write_bytes_with_len(&bytes, bytes_len as i32);
                DataInputAction::ReadBytes(bytes)
            },
            //2 writeBytes / readBytes (array + offset).
            2 => {
                let len = rnd.random_range(0..10000);
                let bytes: Vec<u8> = (0..len).map(|_| rnd.random()).collect();
                let bytes_len = bytes.len();
                let off = if len == 0 {
                    0
                } else {
                    rnd.random_range(0..bytes_len)
                };
                let length = if len == 0 {
                    0
                } else {
                    rnd.random_range(0..(bytes_len - off))
                };
                let _ = dst.write_bytes_range(&bytes, off as i32, length as i32);
                DataInputAction::ReadBytesRange { bytes, off, length }
            },
            //3 writeInt / readInt
            3 => {
                let value: i32 = rnd.random();
                let _ = dst.write_int(value);
                DataInputAction::ReadInt(value)
            },
            //4 writeLong / readInt
            4 => {
                let value: i64 = rnd.random();
                let _ = dst.write_long(value);
                DataInputAction::ReadLong(value)
            },
            //5 writeShort / readShort
            5 => {
                let value: i16 = rnd.random();
                let _ = dst.write_short(value);
                DataInputAction::ReadShort(value)
            },
            //6 writeVInt / readVInt
            6 => {
                let value: i32 = rnd.random();
                let _ = dst.write_vint(value);
                DataInputAction::ReadVInt(value)
            },
            //7 writeZInt / readZInt
            7 => {
                let value: i32 = rnd.random();
                let _ = dst.write_zint(value);
                DataInputAction::ReadZInt(value)
            },
            //8 writeZLong / readZLong
            8 => {
                let value: i64 = rnd.random();
                let _ = dst.write_zlong(value);
                DataInputAction::ReadZLong(value)
            },
            //9 writeVLong / readVLong
            9 => {
                let mut value: i64 = rnd.random();
                value &= (-1i64 as u64 >> 1) as i64;
                let _ = dst.write_vlong(value);
                DataInputAction::ReadVLong(value)
            },
            //10  writeString / readString
            10 => {
                let value = if rnd.random_range(0..50) == 0 {
                    // Occasionally a large blob
                    (0..rnd.random_range(2048..4096))
                        .map(|_| rnd.random::<char>())
                        .collect::<String>()
                } else {
                    (0..rnd.random_range(0..10))
                        .map(|_| rnd.random::<char>())
                        .collect::<String>()
                };
                let _ = dst.write_string(&value);
                DataInputAction::ReadString(value)
            },
            _ => unreachable!("Unexpected generator index"),
        };
        vec.push(action);
    }
    vec
}
