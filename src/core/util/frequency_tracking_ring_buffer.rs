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
use crate::core::util::accountable::Accountable;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use std::cmp;
use std::collections::HashMap;
/// A ring buffer that tracks the frequency of the integers that it contains.
/// This is typically useful to track the hash codes of popular recently-used items.
///
/// This data structure requires about 22 bytes per entry on average (between 16 and 28).
pub struct FrequencyTrackingRingBuffer {
    max_size: usize,
    buffer: Vec<i32>,
    position: usize,
    frequencies: IntBag,
}

impl FrequencyTrackingRingBuffer {
    /// Create a new ring buffer that will contain at most `max_size` items.
    /// This buffer will initially contain `max_size` times the `sentinel` value.
    pub fn new(max_size: usize, sentinel: i32) -> Result<Self> {
        if max_size < 2 {
            return Err(LuceneError::illegal_argument("max_size must be at least 2"));
        }

        let buffer = vec![sentinel; max_size];
        let mut frequencies = IntBag::new(max_size);

        for _ in 0..max_size {
            frequencies.add(sentinel);
        }
        debug_assert_eq!(frequencies.frequency(sentinel) as usize, max_size);

        Ok(Self {
            max_size,
            buffer,
            position: 0,
            frequencies,
        })
    }
    /// Add a new item to this ring buffer, potentially removing the oldest entry
    /// from this buffer if it is already full.
    pub fn add(&mut self, i: i32) {
        // remove the previous value
        let removed = self.buffer[self.position];
        let removed_from_bag = self.frequencies.remove(removed);
        debug_assert!(removed_from_bag);

        // add the new value
        self.buffer[self.position] = i;
        self.frequencies.add(i);

        // increment the position
        self.position += 1;
        if self.position == self.max_size {
            self.position = 0;
        }
    }
    /// Returns the frequency of the provided key in the ring buffer.
    pub fn frequency(&self, key: i32) -> i32 {
        self.frequencies.frequency(key)
    }

    #[cfg(test)]
    pub fn as_frequency_map(&self) -> HashMap<i32, i32> {
        self.frequencies.as_map()
    }
}
impl Accountable for FrequencyTrackingRingBuffer {
    fn ram_bytes_used(&self) -> Result<i64> {
        todo!()
    }
}

pub struct IntBag {
    pub keys: Vec<i32>,
    pub freqs: Vec<i32>,
    pub mask: usize,
}

impl IntBag {
    pub fn new(max_size: usize) -> Self {
        let mut capacity = cmp::max(2, max_size * 3 / 2);
        capacity = capacity.next_power_of_two();
        assert!(capacity > max_size);

        let keys = vec![0; capacity];
        let freqs = vec![0; capacity];
        let mask = capacity - 1;

        Self { keys, freqs, mask }
    }
    /// Return the frequency of the give key in the bag.
    pub fn frequency(&self, key: i32) -> i32 {
        let mut slot = (key as usize) & self.mask;
        loop {
            if self.keys[slot] == key {
                return self.freqs[slot];
            } else if self.freqs[slot] == 0 {
                return 0;
            }
            slot = (slot + 1) & self.mask;
        }
    }
    /// Increment the frequency of the given key by 1 and return its new frequency.
    pub fn add(&mut self, key: i32) -> i32 {
        let mut slot = (key as usize) & self.mask;
        loop {
            if self.freqs[slot] == 0 {
                self.keys[slot] = key;
                self.freqs[slot] = 1;
                return 1;
            } else if self.keys[slot] == key {
                self.freqs[slot] += 1;
                return self.freqs[slot];
            }
            slot = (slot + 1) & self.mask;
        }
    }
    /// Decrement the frequency of the given key by one, or do nothing if the key is not present in
    /// the bag. Returns `true` iff the key was contained in the bag.
    pub fn remove(&mut self, key: i32) -> bool {
        let mut slot = (key as usize) & self.mask;
        loop {
            if self.freqs[slot] == 0 {
                // no such key in the bag
                return false;
            } else if self.keys[slot] == key {
                self.freqs[slot] -= 1;
                let new_freq = self.freqs[slot];
                if new_freq == 0 {
                    // removed
                    self.relocate_adjacent_keys(slot);
                }
                return true;
            }
            slot = (slot + 1) & self.mask;
        }
    }
    fn relocate_adjacent_keys(&mut self, mut free_slot: usize) {
        let mut slot = (free_slot + 1) & self.mask;
        loop {
            let freq = self.freqs[slot];
            if freq == 0 {
                // end of the collision chain, we're done
                break;
            }
            let key = self.keys[slot];
            // the slot where <code>key</code> should be if there were no collisions
            let expected_slot = (key as usize) & self.mask;
            // if the free slot is between the expected slot and the slot where the
            // key is, then we can relocate there
            if Self::between(expected_slot, slot, free_slot) {
                self.keys[free_slot] = key;
                self.freqs[free_slot] = freq;
                // slot becomes the new free slot
                self.freqs[slot] = 0;
                free_slot = slot;
            }
            slot = (slot + 1) & self.mask;
        }
    }
    /// Given a chain of occupied slots between `chain_start` and `chain_end`,
    /// return whether `slot` is between the start and end of the chain.
    fn between(chain_start: usize, chain_end: usize, slot: usize) -> bool {
        if chain_start <= chain_end {
            chain_start <= slot && slot <= chain_end
        } else {
            slot >= chain_start || slot <= chain_end
        }
    }

    pub(crate) fn as_map(&self) -> HashMap<i32, i32> {
        let mut map = HashMap::new();
        for i in 0..self.keys.len() {
            if self.freqs[i] > 0 {
                map.insert(self.keys[i], self.freqs[i]);
            }
        }
        map
    }
}
impl Accountable for IntBag {
    fn ram_bytes_used(&self) -> Result<i64> {
        todo!()
    }
}
#[cfg(test)]
#[cfg(test)]
mod tests {
    use super::*;
    use crate::test::util::lucene_test_case::lucene_test_case_util::random;
    use rand::Rng;
    use std::collections::HashMap;

    fn assert_buffer(
        buffer: &FrequencyTrackingRingBuffer,
        max_size: usize,
        sentinel: i32,
        items: &[i32],
    ) {
        let recent_items = if items.len() <= max_size {
            let mut v = vec![sentinel; max_size - items.len()];
            v.extend_from_slice(items);
            v
        } else {
            items[items.len() - max_size..].to_vec()
        };

        let mut expected_frequencies: HashMap<i32, i32> = HashMap::new();
        for &item in &recent_items {
            *expected_frequencies.entry(item).or_insert(0) += 1;
        }

        assert_eq!(expected_frequencies, buffer.as_frequency_map());
    }
    #[test]
    fn test_frequency_tracking_ring_buffer_randomized() -> Result<()> {
        let mut random = random();
        let iterations = 100 + random.random_range(0..50);

        for _ in 0..iterations {
            let max_size = 2 + random.random_range(0..100);
            let num_items = random.random_range(0..5000);
            let max_item = 1 + random.random_range(0..100);
            let sentinel = random.random_range(0..200);

            let mut items = Vec::with_capacity(num_items);
            let mut buffer = FrequencyTrackingRingBuffer::new(max_size, sentinel)?;

            for _ in 0..num_items {
                let item = random.random_range(0..max_item);
                items.push(item);
                buffer.add(item);
            }

            assert_buffer(&buffer, max_size, sentinel, &items);
        }
        Ok(())
    }

    #[test]
    fn test_ram_bytes_used() {
        todo!()
    }
}
