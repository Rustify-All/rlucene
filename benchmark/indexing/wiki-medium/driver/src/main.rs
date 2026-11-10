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
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::Path;
use std::sync::{Arc, Barrier, Mutex, mpsc};
use std::thread;
use std::time::Instant;

use chrono::{NaiveDateTime, Timelike};
use rlucene::core::analysis::{
  analyzer::Analyzer, reader::ReaderEnum, standard::standard_analyzer::StandardAnalyzer,
  token_stream::TokenStream,
};
use rlucene::core::document::{
  document::Document,
  field::{FieldBase, Store},
  fields::Fields,
  int_point::IntPoint,
  string_field::StringField,
  text_field::TextField,
};
use rlucene::core::index::{
  concurrent_merge_scheduler::ConcurrentMergeScheduler,
  directory_reader,
  index_reader::IndexReader,
  index_writer::IndexWriter,
  index_writer_config::{IndexWriterConfig, OpenMode},
  live_index_writer_config::LiveIndexWriterConfig,
  merge_policy::MergePolicy,
  serial_merge_scheduler::SerialMergeScheduler,
  tiered_merge_policy::TieredMergePolicy,
  two_phase_commit::TwoPhaseCommit,
};
use rlucene::core::store::{
  fs_directory::FSDirectory, mmap_directory::MMapDirectory,
  native_fs_lock_factory::NativeFSLockFactory,
};
use rlucene::core::util::{
  attribute_source::AttributeSource,
  close::{Closeable, CloseableRef},
};
use serde::Deserialize;
use serde_json::json;

type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;

#[derive(Clone, Deserialize)]
struct Config {
  action: String,
  corpus: String,
  documents: usize,
  index: String,
  threads: usize,
  ram_mb: f64,
  max_buffered_docs: i32,
  scheduler: String,
  merge_threads: i32,
  mode: String,
  warmup: usize,
  run_count: usize,
}

fn java_whitespace(c: char) -> bool {
  matches!(c, '\u{0009}'..='\u{000d}' | '\u{001c}'..='\u{0020}' | '\u{1680}' |
    '\u{2000}'..='\u{2006}' | '\u{2008}'..='\u{200a}' | '\u{2028}' | '\u{2029}' | '\u{205f}' | '\u{3000}')
}

fn base36(mut n: usize) -> Result<String> {
  let mut bytes = [b'0'; 6];
  for b in bytes.iter_mut().rev() {
    *b = b"0123456789abcdefghijklmnopqrstuvwxyz"[n % 36];
    n /= 36;
  }
  Ok(String::from_utf8(bytes.to_vec())?)
}

fn new_document() -> Result<Document> {
  let mut d = Document::new();
  d.add(StringField::from_string("title", "", Store::No)?);
  d.add(TextField::from_string("titleTokenized", "", Store::Yes)?);
  d.add(TextField::from_string("body", "", Store::No)?);
  d.add(StringField::from_string("randomLabel", "", Store::No)?);
  d.add(StringField::from_string("id", "", Store::Yes)?);
  d.add(IntPoint::new("id", [0])?);
  d.add(StringField::from_string("date", "", Store::Yes)?);
  d.add(IntPoint::new("timesecnum", [0])?);
  Ok(d)
}

fn fill(d: &mut Document, line: &str, id: usize) -> Result<(u64, u64)> {
  let parts: Vec<_> = line.splitn(5, '\t').collect();
  if parts.len() < 4 {
    return Err(format!("invalid line for document {id}").into());
  }
  let title = parts[0];
  let date = parts[1];
  let body = parts[2];
  let label = parts[3].trim_matches(java_whitespace);
  let dt = NaiveDateTime::parse_from_str(date, "%d-%b-%Y %H:%M:%S%.f")?;
  let second = dt.num_seconds_from_midnight() as i32;
  // Field instances and Document are reused for this thread, in BenchDoc order.
  for (i, field) in d.into_iter().enumerate() {
    match field {
      Fields::String(f) => f.set_string_value(match i {
        0 => title.to_string(),
        3 => label.to_string(),
        4 => base36(id)?,
        6 => date.to_string(),
        _ => return Err("unexpected string field".into()),
      })?,
      Fields::Text(f) => f.set_string_value(if i == 1 { title } else { body })?,
      Fields::IntPoint(f) => f.set_int_values([if i == 5 { id as i32 } else { second }])?,
      _ => return Err("unexpected field type".into()),
    }
  }
  Ok((
    (title.len() + body.len() + label.len()) as u64,
    0, // Verified outside the timed driver by the runner.
  ))
}

fn index(c: &Config) -> Result<()> {
  let directory = Arc::new(MMapDirectory::new(Path::new(&c.index))?);
  let mut iwc = IndexWriterConfig::with_analyzer(StandardAnalyzer::new())?;
  iwc
    .set_open_mode(OpenMode::Create)
    .set_ram_buffer_size_mb(c.ram_mb)
    .set_max_buffered_docs(c.max_buffered_docs)
    .set_use_compound_file(false);
  let mut policy = TieredMergePolicy::new();
  <TieredMergePolicy as MergePolicy<FSDirectory<NativeFSLockFactory, MMapDirectory>>>::get_base_mut(&mut policy)
    .set_no_cfs_ratio(0.0)?;
  iwc.set_merge_policy(policy);
  if c.scheduler == "serial" {
    iwc.set_merge_scheduler(SerialMergeScheduler::new());
  } else {
    let cms = ConcurrentMergeScheduler::new();
    cms.set_max_merges_and_threads(c.merge_threads + 4, c.merge_threads)?;
    cms.enable_auto_io_throttle()?;
    iwc.set_merge_scheduler(cms);
  }
  let writer = Arc::new(IndexWriter::new(directory.clone(), iwc)?);
  let (sender, receiver) = mpsc::sync_channel::<(usize, String)>(1024);
  let receiver = Arc::new(Mutex::new(receiver));
  let corpus = c.corpus.clone();
  let limit = c.documents;
  // As upstream, the bounded input reader starts before the timed release.
  let producer = thread::spawn(move || -> Result<()> {
    let mut lines = BufReader::with_capacity(65536, File::open(corpus)?).lines();
    for id in 0..limit {
      let line = lines.next().ok_or("corpus ended before document limit")??;
      sender.send((id, line))?;
    }
    Ok(())
  });
  let barrier = Arc::new(Barrier::new(c.threads + 1));
  let mut workers = Vec::new();
  // Construct all reusable documents before spawning; construction failure cannot strand a barrier.
  let mut documents = Vec::new();
  for _ in 0..c.threads {
    documents.push(new_document()?);
  }
  for mut doc in documents {
    let w = writer.clone();
    let rx = receiver.clone();
    let gate = barrier.clone();
    workers.push(thread::spawn(move || -> Result<(u64, u64, u64)> {
      gate.wait();
      gate.wait();
      let (mut count, mut bytes, mut chars) = (0, 0, 0);
      loop {
        let item = rx.lock().map_err(|_| "reader queue poisoned")?.recv();
        let (id, line) = match item {
          Ok(item) => item,
          Err(_) => break,
        };
        let (b, ch) = fill(&mut doc, &line, id)?;
        w.add_document(&mut doc)?;
        count += 1;
        bytes += b;
        chars += ch;
      }
      Ok((count, bytes, chars))
    }));
  }
  drop(receiver);
  barrier.wait(); // All workers are ready before timing starts.
  let start = Instant::now();
  barrier.wait();
  let mut totals = (0, 0, 0);
  let mut failure = None;
  for worker in workers {
    match worker.join() {
      Ok(Ok((n, b, ch))) => {
        totals.0 += n;
        totals.1 += b;
        totals.2 += ch;
      },
      Ok(Err(e)) => {
        failure = Some(e);
      },
      Err(_) => {
        failure = Some("index worker panicked".into());
      },
    }
  }
  let produced = producer.join().map_err(|_| "input reader panicked")?;
  let write_ns = start.elapsed().as_nanos();
  if let Some(error) = failure {
    writer.rollback()?;
    return Err(error);
  }
  produced?;
  let mut commit_ns = 0;
  let mut close_ns = 0;
  let mut rollback_ns = 0;
  let tail = Instant::now();
  if c.mode == "full" {
    let t = Instant::now();
    writer.commit()?;
    commit_ns = t.elapsed().as_nanos();
    let t = Instant::now();
    writer.close()?;
    close_ns = t.elapsed().as_nanos();
  } else {
    let t = Instant::now();
    writer.rollback()?;
    rollback_ns = t.elapsed().as_nanos();
  }
  let finish_ns = tail.elapsed().as_nanos();
  let total_ns = start.elapsed().as_nanos();
  if totals.0 != c.documents as u64 {
    return Err("document count mismatch".into());
  }
  directory.close()?;
  println!(
    "{}",
    json!({"documents":totals.0, "utf8_text_bytes":totals.1,
    "luceneutil_utf16_units":totals.2, "write_ns":write_ns, "finish_ns":finish_ns,
    "commit_ns":commit_ns, "close_wait_merges_ns":close_ns, "rollback_ns":rollback_ns,
    "total_ns":total_ns, "persisted":c.mode=="full", "jit_ms":null, "gc_ms":null,
    "gc_count":null})
  );
  Ok(())
}

fn analyze(c: &Config) -> Result<()> {
  let mut lines = BufReader::with_capacity(65536, File::open(&c.corpus)?).lines();
  let analyzer = StandardAnalyzer::new();
  let (mut hash, mut tokens, mut elapsed) = (0i64, 0u64, 0u128);
  for i in 0..(c.warmup + c.run_count) {
    let line = lines.next().ok_or("not enough analyzer input lines")??;
    let start = Instant::now();
    let mut stream = analyzer.token_stream("field", ReaderEnum::from(line))?;
    stream.reset()?;
    while stream.increment_token()? {
      let attrs = stream.get_attribute_source();
      let mut h = 0i32;
      for ch in attrs.buffer()?[..attrs.length()?].iter().rev() {
        let mut units = [0u16; 2];
        for unit in ch.encode_utf16(&mut units).iter().rev() {
          h = h.wrapping_mul(31).wrapping_add(i32::from(*unit));
        }
      }
      // Java's int multiplication wraps before promotion to long.
      hash = hash
        .wrapping_add(i64::from(h.wrapping_mul(31)))
        .wrapping_add(i64::from(attrs.get_position_increment()?.wrapping_mul(31)))
        .wrapping_add(i64::from(attrs.start_offset()?.wrapping_mul(31)))
        .wrapping_add(i64::from(attrs.end_offset()?.wrapping_mul(31)));
      if i >= c.warmup {
        tokens += 1;
      }
    }
    stream.end()?;
    stream.close()?;
    if i >= c.warmup {
      elapsed += start.elapsed().as_nanos();
    }
  }
  println!(
    "{}",
    json!({"hash":hash,"tokens":tokens,"analyze_ns":elapsed,
    "warmup":c.warmup,"run_count":c.run_count})
  );
  Ok(())
}

fn read_index(c: &Config) -> Result<()> {
  let directory = Arc::new(MMapDirectory::new(Path::new(&c.index))?);
  let reader = directory_reader::open(directory.clone())?;
  let n = reader.num_docs()?;
  if n != c.documents as i32 || reader.has_deletions()? {
    return Err("invalid reopened index".into());
  }
  println!("{}", json!({"num_docs":n,"max_doc":reader.max_doc()?}));
  reader.close()?;
  directory.close()?;
  Ok(())
}

fn execute(path: &str) -> Result<()> {
  let config: Config = serde_json::from_reader(File::open(path)?)?;
  match config.action.as_str() {
    "index" => index(&config),
    "analyze" => analyze(&config),
    "read" => read_index(&config),
    _ => Err("unsupported action".into()),
  }
}

fn main() -> Result<()> {
  let path = std::env::args()
    .nth(1)
    .ok_or("usage: indexing-benchmark config.json | --server")?;
  if path == "--server" {
    for line in std::io::stdin().lock().lines() {
      execute(&line?)?;
      std::io::Write::flush(&mut std::io::stdout())?;
    }
    Ok(())
  } else {
    execute(&path)
  }
}
