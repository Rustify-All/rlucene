# Indexing Benchmark

Compare public-API indexing on a fixed million-row sample. Available presets:

- [Rally HTTP logs](../datasets/rally-http-logs/README.md), structured log rows.
- [MS MARCO Passage Ranking v1](../datasets/ms-marco-passage/README.md), short text passages.
- [GeoNames](geonames/README.md), geographic points, populations and multilingual aliases.
- [NYC Yellow Taxi](nyc-yellow-taxi/README.md), trip timestamps, zones and numeric measurements.
- [Wikipedia medium](wiki-medium/README.md), one million luceneutil line documents flushed as a single segment.

## Rally HTTP logs on macOS

Use Python 3.9+, Rust 1.98.1 and JDK 23. From the repository root:

```sh
python3 -B benchmark/run.py indexing datasets
python3 -B benchmark/datasets/prepare.py rally-http-logs-1m
python3 -B benchmark/run.py indexing build
python3 -B benchmark/run.py indexing smoke --dataset rally-http-logs-1m
python3 -B benchmark/run.py indexing run --dataset rally-http-logs-1m
```

`build` compiles the standalone Java and Rust drivers and an independent Rust
index auditor. The Rust driver uses the checkout through a Cargo path dependency.
Use `--java-home` and `--jars-dir` to reuse local dependencies. The runner checks
the pinned Java JAR digests. Use `--data` to select a prepared input directory
and `--output` to select a fresh result directory.

`smoke` writes one full index per selected engine and checks its contents. The
default `run` compares ordinary RLucene release with Java Lucene 10.1.0 on JDK
23, with normal JIT, Parallel GC and a fixed 4 GiB heap.

To reproduce the optimized three-way comparison:

```sh
rustup component add llvm-tools-preview --toolchain 1.98.1
python3 -B benchmark/run.py indexing build-tuned --dataset rally-http-logs-1m
python3 -B benchmark/run.py indexing run --dataset rally-http-logs-1m --rust-profile both
```

`build-tuned` requires the ordinary build above. It builds Rust with native CPU
and ThinLTO, trains PGO with four independent writes of the same million-row
input, then builds the optimized executable from that profile. `--rust-profile
both` measures Java, ordinary Rust and optimized Rust in the same batches.
Use `--rust-profile tuned` to compare Java with the optimized build alone.
Optimized results apply to the machine and the workload used in training.

## What Rally measures

Both writers reuse their document and field objects. A single `KeywordField`
provides postings and SortedSet DocValues for `request.raw`. The full schema
includes analyzed DOCS-only `request`, original JSON, point fields, and
DocValues. The [preset](presets/rally-http-logs-1m.json) fixes one writer thread,
mmap, Lucene101 BEST_SPEED, 256 MiB buffering, flush every 65,536 documents,
no merges, and 16 segments.

- **Write** times binary record decoding, field updates, analysis, indexing
  and automatic flush. File loading and reusable field setup occur before it.
- **Commit** times the explicit durable commit, including the final segment.
- **Close** times writer close after the commit. The three measured phases sum
  to the reported total.

Every batch uses fresh processes, two full warmup writes per engine, then six
balanced rounds. The three-way run uses all six execution orders in each batch.
Preflight checks complete stored values, DocValues, point values, request
postings, CheckIndex level 2 and cross-engine reopen. Each subsequent index is
reopened and compared with the 17-case query oracle. Query checks are outside
the timed write operations. Build, training, validation and timing use the
shared performance lock.

## MS MARCO Passage Ranking v1 on macOS

Use Python 3.9+, Rust 1.98.1 and JDK 23. From the repository root:

```sh
python3 -B benchmark/datasets/prepare.py ms-marco-passage-1m
python3 -B benchmark/run.py indexing smoke --dataset ms-marco-passage-1m
rustup component add llvm-tools-preview --toolchain 1.98.1
python3 -B benchmark/run.py indexing run --dataset ms-marco-passage-1m --rust-profile both
```

The default smoke check builds the two drivers and verifies a 14-row
boundary fixture and 4,096 source passages. Adding `--rust-profile both`
to smoke also builds the optimized Rust executable and verifies the million-row
indexes. The full run builds and validates before timing under one shared
lock. It uses normal Java JIT, Parallel GC and a fixed 4 GiB heap. The optimized
Rust build uses native CPU, ThinLTO and a profile trained on four independent
writes of the same million passages. Only the workload process contributes
to the merged profile. Training time and raw profile files are recorded
separately. Use `--data`, `--output`, `--java-home` and `--jars-dir` as needed.

Both writers reuse one document and its fields. The stored `pid` is a
`StringField` with DOCS postings and omitted norms. The stored `passage` is
a `TextField` using StandardAnalyzer, with frequencies, positions and norms.
The [preset](presets/ms-marco-passage-1m.json) uses one writer, mmap, Lucene101
BEST_SPEED, 256 MiB buffering, a 65,536-document flush, no merges and 16
segments. The write, explicit commit and writer close boundaries match Rally.
Full refers to the selected million passages. Use a fresh output directory
and allow at least 15 GB for the live indexes and build files.

Preflight compares Unicode tokens, position increments and UTF-16 offsets
on the fixture and a subset of the small sample. It checks all stored values
against the input and compares the complete postings and norms across
engines. Java CheckIndex level 2 and cross-engine reopen cover each preflight
index. Queries compare exact counts and ordered BM25 Top 10 external pids,
docIDs and f32 score bits. Ties use ascending docID. The query set contains
exact pid, term and phrase checks and 32 official dev queries analyzed into
SHOULD term clauses. These checks run outside timing.

Every full batch starts fresh processes, with two warmup writes per engine
and six formal rounds. The three-way comparison uses all six execution
orders per batch and 18 formal groups across three batches. Java reopens
each warmup and formal index for the 37-query check. Verified timed indexes
are removed, and the preflight indexes and original observations are kept.

## GeoNames and NYC Yellow Taxi on macOS

The [GeoNames guide](geonames/README.md) and [NYC Yellow Taxi guide](nyc-yellow-taxi/README.md)
cover data preparation, checksums and run commands. Both runners use the RLucene
commit recorded in the preset. Use a full Git clone so that the runner can extract
and verify the measured revision even when the checked-out benchmark code is newer.
The Java runner uses Lucene 10.1.0 on JDK 23 with normal JIT, Parallel GC and a
fixed 4 GiB heap. The optimized Rust build uses native CPU, ThinLTO, 16 codegen
units and a profile trained by one process writing four full indexes for that workload.

The full run writes one million documents with one writer thread, mmap,
Lucene101 BEST_SPEED, 256 MiB buffering, a flush threshold of 65,536 documents,
and merges, compound files and implicit commit on close disabled. It times the
write loop, explicit commit and writer close separately. Index initialization,
input loading, directory close and all verification happen outside those timers.

The fixture and 4,096-row preflights compare Java and ordinary Rust. The million-row
preflight also includes optimized Rust. Each preflight index passes Java CheckIndex
level 2 and is reopened by each participating reader for a full field audit and a
comparison against the independent query oracle. Three batches each launch fresh
writer processes, warm up twice per engine, and run all six permutations of Java,
ordinary Rust and optimized Rust. Java and ordinary Rust reopen every warmup and
formal index for query verification. The runner retains logs and samples and removes
the temporary timed indexes after verification.

## Wikipedia medium on macOS

The [Wikipedia medium guide](wiki-medium/README.md) records the pinned input,
the exact Rust and Java drivers, and the six-order comparison between
RLucene `534c6d9c0`, RLucene `c2f203337`, and Apache Lucene 10.1.0.

## Read the results

The default result directory is `benchmark/runs/indexing/`. Keep `RUN.json`,
`samples.jsonl`, `verification.jsonl` and any logs or indexes needed for
auditing. Rally build receipts live in `benchmark/.cache/indexing/`.
MS MARCO keeps the build logs, binary fingerprints and optional PGO
training receipts with each run.

`RESULT.json` contains per-batch and combined paired ratios and absolute
differences. `REPORT.md` gives the measured phase medians. Both can be
regenerated from a complete run:

```sh
python3 -B benchmark/run.py indexing summarize --output /path/to/completed-run
```
