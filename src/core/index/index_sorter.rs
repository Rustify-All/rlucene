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
use crate::core::index::leaf_reader::LeafReader;
use crate::core::index::numeric_doc_values::NumericDocValues;
use crate::core::index::sorted_doc_values::SortedDocValues;
use crate::core::search::doc_id_set_iterator::DocIdSetIterator;
use crate::core::search::doc_id_set_iterator::disi_const::NO_MORE_DOCS;
use crate::core::search::sort_field::MissingValueEnum;
use crate::core::util::ToInt;
use crate::core::util::error::lucene_error::{LuceneError, Result};

pub trait IndexSorter {
    fn get_provider_name(&self) -> &str;

    type DocComparator: DocComparator;
    fn get_doc_comparator<LR>(
        &mut self,
        leaf_reader: &mut LR,
        max_doc: i32,
    ) -> Result<Self::DocComparator>
    where
        LR: LeafReader;
}

// DoubleSorter
/// Sorts documents based on double values from a NumericDocValues instance.
pub struct DoubleSorter<NP> {
    provider_name: String,
    missing_value: Option<f64>,
    reverse_mul: i32,
    values_provider: NP,
}
impl<NP> DoubleSorter<NP>
where
    NP: NumericDocValuesProvider,
{
    pub fn new(
        provider_name: String,
        missing_value: Option<MissingValueEnum>,
        reverse: bool,
        values_provider: NP,
    ) -> Result<Self> {
        let missing_value = if let Some(mv) = missing_value {
            match mv {
                MissingValueEnum::Double(value) => Some(value),
                _ => {
                    return Err(LuceneError::illegal_state(
                        "Missing value type mismatch for Double.",
                    ));
                },
            }
        } else {
            None
        };
        Ok(Self {
            provider_name,
            missing_value,
            reverse_mul: if reverse { -1 } else { 1 },
            values_provider,
        })
    }
}
impl<NP> IndexSorter for DoubleSorter<NP>
where
    NP: NumericDocValuesProvider,
{
    fn get_provider_name(&self) -> &str {
        &self.provider_name
    }

    type DocComparator = DocComparatorImplDouble;

    fn get_doc_comparator<LR>(
        &mut self,
        leaf_reader: &mut LR,
        max_doc: i32,
    ) -> Result<Self::DocComparator>
    where
        LR: LeafReader,
    {
        let mut dvs = self.values_provider.get(leaf_reader)?;
        let mut values = vec![0f64; max_doc as usize];
        if self.missing_value.is_some() {
            values.fill(*self.missing_value.as_ref().unwrap())
        }
        loop {
            let doc_id = dvs.next_doc()?;
            if doc_id == NO_MORE_DOCS {
                break;
            }
            values[doc_id as usize] = f64::from_bits(dvs.long_value()? as u64);
        }
        Ok(DocComparatorImplDouble::new(values, self.reverse_mul))
    }
}
pub struct DocComparatorImplDouble {
    values: Vec<f64>,
    reverse_mul: i32,
}
impl DocComparatorImplDouble {
    pub fn new(values: Vec<f64>, reverse_mul: i32) -> Self {
        Self {
            values,
            reverse_mul,
        }
    }
}
impl DocComparator for DocComparatorImplDouble {
    fn compare(&self, doc_id1: i32, doc_id2: i32) -> i32 {
        self.reverse_mul
            * self.values[doc_id1 as usize]
                .total_cmp(&self.values[doc_id2 as usize])
                .to_int()
    }
}

// IntSorter
/// Sorts documents based on integer values from a NumericDocValues instance  */
pub struct IntSorter<NP> {
    provider_name: String,
    missing_value: Option<i32>,
    reverse_mul: i32,
    values_provider: NP,
}
impl<NP> IntSorter<NP>
where
    NP: NumericDocValuesProvider,
{
    pub fn new(
        provider_name: String,
        missing_value: Option<MissingValueEnum>,
        reverse: bool,
        values_provider: NP,
    ) -> Result<Self> {
        let missing_value = if let Some(mv) = missing_value {
            match mv {
                MissingValueEnum::Int(value) => Some(value),
                _ => {
                    return Err(LuceneError::illegal_state(
                        "Missing value type mismatch for INT.",
                    ));
                },
            }
        } else {
            None
        };
        Ok(Self {
            provider_name,
            missing_value,
            reverse_mul: if reverse { -1 } else { 1 },
            values_provider,
        })
    }
}
impl<NP> IndexSorter for IntSorter<NP>
where
    NP: NumericDocValuesProvider,
{
    fn get_provider_name(&self) -> &str {
        &self.provider_name
    }

    type DocComparator = DocComparatorImplInt;

    fn get_doc_comparator<LR>(
        &mut self,
        leaf_reader: &mut LR,
        max_doc: i32,
    ) -> Result<Self::DocComparator>
    where
        LR: LeafReader,
    {
        let mut dvs = self.values_provider.get(leaf_reader)?;
        let mut values = vec![0i32; max_doc as usize];
        if let Some(mv) = self.missing_value {
            values.fill(mv);
        }
        loop {
            let doc_id = dvs.next_doc()?;
            if doc_id == NO_MORE_DOCS {
                break;
            }
            values[doc_id as usize] = dvs.long_value()? as i32;
        }
        Ok(DocComparatorImplInt::new(values, self.reverse_mul))
    }
}
pub struct DocComparatorImplInt {
    values: Vec<i32>,
    reverse_mul: i32,
}
impl DocComparatorImplInt {
    pub fn new(values: Vec<i32>, reverse_mul: i32) -> Self {
        Self {
            values,
            reverse_mul,
        }
    }
}
impl DocComparator for DocComparatorImplInt {
    fn compare(&self, doc_id1: i32, doc_id2: i32) -> i32 {
        self.reverse_mul
            * self.values[doc_id1 as usize]
                .cmp(&self.values[doc_id2 as usize])
                .to_int()
    }
}

// LongSorter
/// Sorts documents based on long values from a NumericDocValues instance
pub struct LongSorter<NP> {
    provider_name: String,
    missing_value: Option<i64>,
    reverse_mul: i32,
    values_provider: NP,
}
impl<NP> LongSorter<NP>
where
    NP: NumericDocValuesProvider,
{
    pub fn new(
        provider_name: String,
        missing_value: Option<MissingValueEnum>,
        reverse: bool,
        values_provider: NP,
    ) -> Result<Self> {
        let missing_value = if let Some(mv) = missing_value {
            match mv {
                MissingValueEnum::Long(value) => Some(value),
                _ => {
                    return Err(LuceneError::illegal_state(
                        "Missing value type mismatch for Long.",
                    ));
                },
            }
        } else {
            None
        };
        Ok(Self {
            provider_name,
            missing_value,
            reverse_mul: if reverse { -1 } else { 1 },
            values_provider,
        })
    }
}

impl<NP> IndexSorter for LongSorter<NP>
where
    NP: NumericDocValuesProvider,
{
    fn get_provider_name(&self) -> &str {
        &self.provider_name
    }

    type DocComparator = DocComparatorImplLong;

    fn get_doc_comparator<LR>(
        &mut self,
        leaf_reader: &mut LR,
        max_doc: i32,
    ) -> Result<Self::DocComparator>
    where
        LR: LeafReader,
    {
        let mut dvs = self.values_provider.get(leaf_reader)?;
        let mut values = vec![0i64; max_doc as usize];
        if let Some(mv) = self.missing_value {
            values.fill(mv);
        }
        loop {
            let doc_id = dvs.next_doc()?;
            if doc_id == NO_MORE_DOCS {
                break;
            }
            values[doc_id as usize] = dvs.long_value()?;
        }
        Ok(DocComparatorImplLong::new(values, self.reverse_mul))
    }
}

pub struct DocComparatorImplLong {
    values: Vec<i64>,
    reverse_mul: i32,
}

impl DocComparatorImplLong {
    pub fn new(values: Vec<i64>, reverse_mul: i32) -> Self {
        Self {
            values,
            reverse_mul,
        }
    }
}

impl DocComparator for DocComparatorImplLong {
    fn compare(&self, doc_id1: i32, doc_id2: i32) -> i32 {
        self.reverse_mul
            * self.values[doc_id1 as usize]
                .cmp(&self.values[doc_id2 as usize])
                .to_int()
    }
}

// FloatSorter
/// Sorts documents based on float values from a NumericDocValues instance
pub struct FloatSorter<NP> {
    provider_name: String,
    missing_value: Option<f32>,
    reverse_mul: i32,
    values_provider: NP,
}

impl<NP> FloatSorter<NP>
where
    NP: NumericDocValuesProvider,
{
    pub fn new(
        provider_name: String,
        missing_value: Option<MissingValueEnum>,
        reverse: bool,
        values_provider: NP,
    ) -> Result<Self> {
        let missing_value = if let Some(mv) = missing_value {
            match mv {
                MissingValueEnum::Float(value) => Some(value),
                _ => {
                    return Err(LuceneError::illegal_state(
                        "Missing value type mismatch for Float.",
                    ));
                },
            }
        } else {
            None
        };
        Ok(Self {
            provider_name,
            missing_value,
            reverse_mul: if reverse { -1 } else { 1 },
            values_provider,
        })
    }
}

impl<NP> IndexSorter for FloatSorter<NP>
where
    NP: NumericDocValuesProvider,
{
    fn get_provider_name(&self) -> &str {
        &self.provider_name
    }

    type DocComparator = DocComparatorImplFloat;

    fn get_doc_comparator<LR>(
        &mut self,
        leaf_reader: &mut LR,
        max_doc: i32,
    ) -> Result<Self::DocComparator>
    where
        LR: LeafReader,
    {
        let mut dvs = self.values_provider.get(leaf_reader)?;
        let mut values = vec![0f32; max_doc as usize];
        if let Some(mv) = self.missing_value {
            values.fill(mv);
        }
        loop {
            let doc_id = dvs.next_doc()?;
            if doc_id == NO_MORE_DOCS {
                break;
            }
            let bits = dvs.long_value()? as u32;
            values[doc_id as usize] = f32::from_bits(bits);
        }
        Ok(DocComparatorImplFloat::new(values, self.reverse_mul))
    }
}

pub struct DocComparatorImplFloat {
    values: Vec<f32>,
    reverse_mul: i32,
}

impl DocComparatorImplFloat {
    pub fn new(values: Vec<f32>, reverse_mul: i32) -> Self {
        Self {
            values,
            reverse_mul,
        }
    }
}

impl DocComparator for DocComparatorImplFloat {
    fn compare(&self, doc_id1: i32, doc_id2: i32) -> i32 {
        let v1 = self.values[doc_id1 as usize];
        let v2 = self.values[doc_id2 as usize];
        let ord = v1.total_cmp(&v2).to_int();
        self.reverse_mul * ord
    }
}

// StringSorter
/// Sorts documents based on short values from a NumericDocValues instance
pub struct StringSorter<SP> {
    provider_name: String,
    missing_value: Option<MissingValueEnum>,
    reverse_mul: i32,
    values_provider: SP,
}

impl<SP> StringSorter<SP>
where
    SP: SortedDocValuesProvider,
{
    pub fn new(
        provider_name: String,
        missing_value: Option<MissingValueEnum>,
        reverse: bool,
        values_provider: SP,
    ) -> Self {
        Self {
            provider_name,
            missing_value,
            reverse_mul: if reverse { -1 } else { 1 },
            values_provider,
        }
    }
}

impl<SP> IndexSorter for StringSorter<SP>
where
    SP: SortedDocValuesProvider,
{
    fn get_provider_name(&self) -> &str {
        &self.provider_name
    }

    type DocComparator = DocComparatorImplString;

    fn get_doc_comparator<LR>(
        &mut self,
        leaf_reader: &mut LR,
        max_doc: i32,
    ) -> Result<Self::DocComparator>
    where
        LR: LeafReader,
    {
        let mut sorted = self.values_provider.get(leaf_reader)?;
        let missing_ord = match self.missing_value {
            Some(MissingValueEnum::StringLast) => i32::MAX,
            _ => i32::MIN,
        };

        let mut ords = vec![missing_ord; max_doc as usize];
        let mut doc_id;
        loop {
            doc_id = sorted.next_doc()?;
            if doc_id == NO_MORE_DOCS {
                break;
            }
            ords[doc_id as usize] = sorted.ord_value()?;
        }
        Ok(DocComparatorImplString::new(ords, self.reverse_mul))
    }
}

pub struct DocComparatorImplString {
    ords: Vec<i32>,
    reverse_mul: i32,
}

impl DocComparatorImplString {
    pub fn new(ords: Vec<i32>, reverse_mul: i32) -> Self {
        Self { ords, reverse_mul }
    }
}

impl DocComparator for DocComparatorImplString {
    fn compare(&self, doc_id1: i32, doc_id2: i32) -> i32 {
        let o1 = self.ords[doc_id1 as usize];
        let o2 = self.ords[doc_id2 as usize];
        let cmp = o1.cmp(&o2).to_int();
        self.reverse_mul * cmp
    }
}

/// Used for sorting documents across segments
pub trait ComparableProvider {
    /// Returns a long so that the natural ordering of long values matches the ordering of doc IDs for the given comparator
    fn get_as_comparable_long(&mut self, doc_id: i32) -> Result<i64>;
}
/// A comparator of doc IDs, used for sorting documents within a segment
pub trait DocComparator {
    /// Compare docID1 against docID2.
    fn compare(&self, doc_id1: i32, doc_id2: i32) -> i32;
}
/// Provide a NumericDocValues instance for a LeafReader
pub trait NumericDocValuesProvider {
    /// Returns the numeric value for the given doc ID.
    type NumericDocValues<LR>: NumericDocValues
    where
        LR: LeafReader;
    /// Returns the NumericDocValues instance for this LeafReader
    fn get<LR>(&mut self, leaf_reader: &mut LR) -> Result<Self::NumericDocValues<LR>>
    where
        LR: LeafReader;
}
/// Provide a SortedDocValues instance for a LeafReader
pub trait SortedDocValuesProvider {
    type SortedDocValues<LR>: SortedDocValues
    where
        LR: LeafReader;
    /// Returns the SortedDocValues instance for this LeafReader
    fn get<LR>(&mut self, leaf_reader: &mut LR) -> Result<Self::SortedDocValues<LR>>
    where
        LR: LeafReader;
}

pub enum DocComparatorEnum {
    Int(DocComparatorImplInt),
    Long(DocComparatorImplLong),
    Float(DocComparatorImplFloat),
    Double(DocComparatorImplDouble),
    String(DocComparatorImplString),
}
impl DocComparator for DocComparatorEnum {
    fn compare(&self, doc_id1: i32, doc_id2: i32) -> i32 {
        match self {
            DocComparatorEnum::Int(cmp) => cmp.compare(doc_id1, doc_id2),
            DocComparatorEnum::Long(cmp) => cmp.compare(doc_id1, doc_id2),
            DocComparatorEnum::Float(cmp) => cmp.compare(doc_id1, doc_id2),
            DocComparatorEnum::Double(cmp) => cmp.compare(doc_id1, doc_id2),
            DocComparatorEnum::String(cmp) => cmp.compare(doc_id1, doc_id2),
        }
    }
}

// DocComparator
pub enum Either2DocComparator<A, B> {
    A(A),
    B(B),
}

impl<A, B> DocComparator for Either2DocComparator<A, B>
where
    A: DocComparator,
    B: DocComparator,
{
    fn compare(&self, doc_id1: i32, doc_id2: i32) -> i32 {
        match self {
            Either2DocComparator::A(t) => t.compare(doc_id1, doc_id2),
            Either2DocComparator::B(s) => s.compare(doc_id1, doc_id2),
        }
    }
}

#[cfg(test)]
mod tests {
    use rand::Rng;

    use crate::core::index::{BytesRef, BytesRefBuilder};
    use crate::core::util::bytes_ref_comparator::{BytesRefComparator, Natural};
    use crate::core::util::error::lucene_error::Result;
    use crate::core::util::stable_string_sorter::{StableStringSorter, StableStringSorterBase};
    use crate::core::util::{
        Comparator, MSBRadixSorterBase, NaturalOrder, SliceCopyOps, Sorter, StringSorter,
        StringSorterBase,
    };
    use crate::test::util::common_method::assert_vecs_equal;
    use crate::test::util::lucene_test_case::lucene_test_case_util::{at_least, random};
    use crate::test::util::test_util::TestUtil;

    #[allow(dead_code)] // for quick search
    struct TestStringSorter;

    fn test(refs: Vec<BytesRef<Vec<u8>>>, len: usize) -> Result<()> {
        test_impl(refs.clone(), len, Natural::default())?;
        test_impl(refs.clone(), len, NaturalOrder::default())?;
        test_stable(refs.clone(), len, Natural::default())?;
        test_stable(refs.clone(), len, NaturalOrder::default())?;
        Ok(())
    }

    fn test_impl(
        refs: Vec<BytesRef<Vec<u8>>>,
        len: usize,
        comparator: impl BytesRefComparator + Comparator<BytesRef<Vec<u8>>>,
    ) -> Result<()> {
        let mut expected: Vec<BytesRef<Vec<u8>>> = refs.clone();
        expected.sort();
        let delegate_sorter = StringSorterTestImpl::new(refs.clone());
        let mut string_sorter = StringSorter::new(delegate_sorter, comparator);
        string_sorter.sort(0, len as i32)?;

        assert_vecs_equal(&expected, &string_sorter.get_delegate_sorter().refs);
        Ok(())
    }

    fn test_stable(
        refs: Vec<BytesRef<Vec<u8>>>,
        len: usize,
        comparator: impl BytesRefComparator + Comparator<BytesRef<Vec<u8>>>,
    ) -> Result<()> {
        let mut expected: Vec<BytesRef<Vec<u8>>> = refs[..len].to_vec();
        let mut actual = refs[..len].to_vec();
        expected.sort();

        let actual_before_sorted = actual.clone();
        let mut ord: Vec<i32> = (0..len).map(|i| i as i32).collect();
        let ord_len = ord.len();
        let delegate_sorter = StableStringSorterTestImpl {
            tmp: vec![0; ord_len],
            ord: &mut ord,
            refs: &mut actual,
        };
        let string_sorter = StableStringSorter::new(delegate_sorter);
        let mut stable_string_sorter = StringSorter::new(string_sorter, comparator);
        stable_string_sorter.sort(0, len as i32)?;
        // `actual` is not sorted, but `ord` is sorted
        assert_vecs_equal(&actual_before_sorted, &actual);
        for i in 0..len {
            assert_eq!(
                &expected[i], &refs[ord[i] as usize],
                "Mismatch at index {}: expected {:?}, found {:?}",
                i, &expected[i], &refs[ord[i] as usize]
            );

            if i > 0 && expected[i] == expected[i - 1] {
                assert!(
                    ord[i] > ord[i - 1],
                    "Not stable: ord[{}] <= ord[{}]",
                    i,
                    i - 1
                );
            }
        }

        Ok(())
    }

    #[test]
    fn test_empty() -> Result<()> {
        let mut random = random();
        let len = random.random_range(0..5);
        let refs: Vec<BytesRef<Vec<u8>>> = (0..len).map(|_| BytesRef::default()).collect();
        test(refs, 0)
    }

    #[test]
    fn test_one_value() -> Result<()> {
        let mut random = random();
        let bytes = BytesRef::from_string(&TestUtil::random_simple_string(&mut random));
        test(vec![bytes], 1)
    }

    #[test]
    fn test_two_values() -> Result<()> {
        let mut random = random();
        let bytes1 = BytesRef::from_string(&TestUtil::random_simple_string(&mut random));
        let bytes2 = BytesRef::from_string(&TestUtil::random_simple_string(&mut random));
        test(vec![bytes1, bytes2], 2)
    }

    fn test_random_impl<R: Rng + ?Sized>(
        common_prefix_len: usize,
        max_len: usize,
        random: &mut R,
    ) -> Result<()> {
        let mut common_prefix = vec![0u8; common_prefix_len];
        random.fill_bytes(&mut common_prefix);
        let len = random.random_range(0..100000);

        let mut bytes: Vec<BytesRef<Vec<u8>>> =
            Vec::with_capacity(len + random.random_range(0..50));
        for _ in 0..len {
            let mut b = vec![0u8; common_prefix_len + random.random_range(0..max_len)];
            random.fill_bytes(&mut b[common_prefix_len..]);
            b.copy_from(&common_prefix, 0);
            bytes.push(BytesRef::from_bytes(b));
        }

        test(bytes, len)
    }
    #[test]
    fn test_random() -> Result<()> {
        let mut random = random();
        let num_iters = at_least(&mut random, 3);
        for _ in 0..num_iters {
            test_random_impl(0, 10, &mut random)?;
        }
        Ok(())
    }
    #[test]
    fn test_random_with_lots_of_duplicates() -> Result<()> {
        let mut random = random();
        let num_iters = at_least(&mut random, 3);
        for _ in 0..num_iters {
            test_random_impl(0, 2, &mut random)?;
        }
        Ok(())
    }
    #[test]
    fn test_random_with_shared_prefix() -> Result<()> {
        let mut random = random();
        let num_iters = at_least(&mut random, 3);
        for _ in 0..num_iters {
            let shared_prefix_len = TestUtil::next_int(&mut random, 1, 30) as usize;
            test_random_impl(shared_prefix_len, 10, &mut random)?;
        }
        Ok(())
    }
    #[test]
    fn test_random_with_shared_prefix_and_lots_of_duplicates() -> Result<()> {
        let mut random = random();
        let num_iters = at_least(&mut random, 3);
        for _ in 0..num_iters {
            let shared_prefix_len = TestUtil::next_int(&mut random, 1, 30) as usize;
            test_random_impl(shared_prefix_len, 2, &mut random)?;
        }
        Ok(())
    }

    struct StringSorterTestImpl {
        refs: Vec<BytesRef<Vec<u8>>>,
    }

    impl StringSorterTestImpl {
        fn new(refs: Vec<BytesRef<Vec<u8>>>) -> Self {
            Self { refs }
        }
    }
    impl Sorter for StringSorterTestImpl {
        fn swap(&mut self, i: i32, j: i32) -> Result<()> {
            self.refs.swap(i as usize, j as usize);
            Ok(())
        }
    }
    impl StringSorterBase for StringSorterTestImpl {
        fn get(
            &mut self,
            _builder: &mut BytesRefBuilder<Vec<u8>>,
            result: &mut BytesRef<Vec<u8>>,
            i: i32,
        ) -> Result<()> {
            let ref_item = &self.refs[i as usize];
            result.offset = ref_item.offset;
            result.length = ref_item.length;
            result.bytes = ref_item.bytes.clone();
            Ok(())
        }
    }

    struct StableStringSorterTestImpl<'a> {
        tmp: Vec<i32>,
        ord: &'a mut Vec<i32>,
        refs: &'a mut [BytesRef<Vec<u8>>],
    }

    impl StringSorterBase for StableStringSorterTestImpl<'_> {
        fn get(
            &mut self,
            _builder: &mut BytesRefBuilder<Vec<u8>>,
            result: &mut BytesRef<Vec<u8>>,
            i: i32,
        ) -> Result<()> {
            let ref_item = &self.refs[self.ord[i as usize] as usize];
            result.offset = ref_item.offset;
            result.length = ref_item.length;
            result.bytes = ref_item.bytes.clone();
            Ok(())
        }
    }

    impl StableStringSorterBase for StableStringSorterTestImpl<'_> {
        fn save(&mut self, i: i32, j: i32) {
            self.tmp[j as usize] = self.ord[i as usize];
        }

        fn restore(&mut self, i: i32, j: i32) {
            self.ord
                .copy_from(&self.tmp[i as usize..j as usize], i as usize);
        }
    }
    impl Sorter for StableStringSorterTestImpl<'_> {
        fn swap(&mut self, i: i32, j: i32) -> Result<()> {
            self.ord.swap(i as usize, j as usize);
            Ok(())
        }
    }
    impl MSBRadixSorterBase for StableStringSorterTestImpl<'_> {}
}
