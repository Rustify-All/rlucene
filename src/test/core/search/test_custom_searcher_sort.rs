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
use crate::core::document::date_tools::{DateTools, Resolution};
use crate::core::document::document::Document;
use crate::core::document::field::Store;
use crate::core::document::sorted_doc_values_field::SortedDocValuesField;
use crate::core::document::string_field::StringField;
use crate::core::document::text_field::TextField;
use crate::core::index::BytesRef;
use crate::core::index::index_reader::{IndexReader, IndexReaderContextType};
use crate::core::index::term::Term;
use crate::core::search::index_searcher::{IndexSearcher, IndexSearcherHook};
use crate::core::search::query::Query;
use crate::core::search::score_doc::ScoreDocLike;
use crate::core::search::sort::Sort;
use crate::core::search::sort_field::{SortField, SortFieldType};
use crate::core::search::term_query::TermQuery;
use crate::core::search::top_docs::TopDocsLike;
use crate::core::util::error::lucene_error::Result;
use crate::test_framework::core::index::random_index_writer::RandomIndexWriter;
use crate::test_framework::core::search::test_custom_searcher_sort::CustomSearcher;
use crate::test_framework::core::util::DefaultCRReader;
use crate::test_framework::core::util::lucene_test_case::{
  at_least_usize, new_directory_shared, random,
};
use chrono::{DateTime, Local, LocalResult, NaiveDate, TimeDelta, TimeZone};
use rand::{Rng, RngExt};
use std::collections::BTreeMap;

#[allow(dead_code)] // for quick search
pub struct TestCustomSearcherSort;

fn set_up<R: Rng + ?Sized>(random: &mut R) -> Result<(DefaultCRReader, Query, usize)> {
  let index_size = at_least_usize(random, 2000);
  let index = new_directory_shared(random)?;

  let writer = RandomIndexWriter::new(random, index)?;
  let mut random_gen = RandomGen::new(random);

  for i in 0..index_size {
    let mut doc = Document::new();

    if i % 5 != 0 {
      doc.add(SortedDocValuesField::new(
        "publicationDate_",
        BytesRef::from_string(&random_gen.get_lucene_date()?),
      ));
    }

    if i % 7 == 0 {
      doc.add(TextField::from_string("content", "test", Store::Yes)?);
    }

    doc.add(StringField::from_string(
      "mandant",
      (i % 3).to_string(),
      Store::Yes,
    )?);

    writer.add_document(&mut *random_gen.random, doc)?;
  }
  let reader = writer.get_reader(&mut *random_gen.random)?;
  writer.close(&mut *random_gen.random)?;
  let query = TermQuery::new(Term::from_text("content", "test")).into();
  Ok((reader, query, index_size))
}

#[test]
fn test_field_sort_custom_searcher() -> Result<()> {
  let mut random = random();
  let (reader, query, _) = set_up(&mut random)?;
  let cust_sort = Sort::with_fields(vec![
    SortField::new(Some("publicationDate_"), SortFieldType::String)?,
    SortField::get_field_score()?,
  ])?;
  let searcher = IndexSearcher::new(reader.get_context()?)?
    .with_hook(IndexSearcherHook::CustomSearcher(CustomSearcher::new(2)));
  match_hits(&searcher, &query, cust_sort)
}

#[test]
fn test_field_sort_single_searcher() -> Result<()> {
  let mut random = random();
  let (reader, query, _) = set_up(&mut random)?;
  let cust_sort = Sort::with_fields(vec![
    SortField::new(Some("publicationDate_"), SortFieldType::String)?,
    SortField::get_field_score()?,
  ])?;
  let searcher = IndexSearcher::new(reader.get_context()?)?
    .with_hook(IndexSearcherHook::CustomSearcher(CustomSearcher::new(2)));
  match_hits(&searcher, &query, cust_sort)
}
fn match_hits(
  searcher: &IndexSearcher<IndexReaderContextType<DefaultCRReader>>,
  query: &Query,
  sort: Sort,
) -> Result<()> {
  let hits_by_rank = searcher.search(query.clone(), usize::MAX)?.score_docs;
  check_hits(hits_by_rank.as_slice(), "Sort by rank: ");

  let mut result_map = BTreeMap::new();
  for (hit_id, hit) in hits_by_rank.iter().enumerate() {
    result_map.insert(hit.doc, hit_id);
  }

  let result_sort = searcher.search_with_sort(query.clone(), usize::MAX, sort)?;
  let v = result_sort.base.score_docs();
  check_hits(v, "Sort by custom criteria: ");

  for hit in result_sort.score_docs() {
    assert!(
      result_map.remove(&hit.doc()).is_some(),
      "sorted hit doc {} was not present in rank-sorted hits",
      hit.doc()
    );
  }

  assert_eq!(0, result_map.len());
  Ok(())
}

fn check_hits<T>(hits: &[T], prefix: &str)
where
  T: ScoreDocLike,
{
  let mut id_map = BTreeMap::new();
  for (doc_num, sd) in hits.iter().enumerate() {
    if let Some(previous) = id_map.insert(sd.doc(), doc_num) {
      panic!(
        "{prefix}Duplicate key for hit index = {doc_num}, previous index = {previous}, Lucene ID = {sd}"
      );
    }
  }
}

struct RandomGen<'a, R: Rng + ?Sized> {
  random: &'a mut R,
  // we use the default Locale/TZ since LuceneTestCase randomizes it
  base: DateTime<Local>,
}

impl<'a, R: Rng + ?Sized> RandomGen<'a, R> {
  fn new(random: &'a mut R) -> Self {
    let now = Local::now();
    let local_base = NaiveDate::from_ymd_opt(1980, 2, 1)
      .map(|date| date.and_time(now.time()))
      .expect("1980-02-01 must be a valid date in the default time zone");
    let base = match Local.from_local_datetime(&local_base) {
      LocalResult::Single(base) => base,
      LocalResult::Ambiguous(_, latest) => latest,
      LocalResult::None => {
        let day = TimeDelta::days(1);
        let before = Local
          .from_local_datetime(&(local_base - day))
          .latest()
          .expect("the preceding local date must be valid");
        let after = Local
          .from_local_datetime(&(local_base + day))
          .latest()
          .expect("the following local date must be valid");
        let offset_change = after.offset().local_minus_utc() - before.offset().local_minus_utc();
        Local
          .from_local_datetime(&(local_base + TimeDelta::seconds(offset_change.into())))
          .latest()
          .expect("the leniently adjusted local date must be valid")
      },
    };
    Self { random, base }
  }

  // Just to generate some different Lucene Date strings
  fn get_lucene_date(&mut self) -> Result<String> {
    DateTools::time_to_string(
      self.base.timestamp_millis() + (self.random.random::<i32>() as i64 - i32::MIN as i64),
      Resolution::DAY,
    )
  }
}
