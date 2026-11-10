# Indexing Benchmark

Compare public-API indexing on one selected dataset. The first preset,
[`rally-http-logs-1m`](../datasets/rally-http-logs/README.md), is a fixed million-row
sample of the Elastic Rally HTTP logs. Other indexing datasets can use
separate presets and drivers.

## Run on macOS

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

## What the preset measures

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

## Read the results

The default result directory is `benchmark/runs/indexing/`. Keep `RUN.json`,
`samples.jsonl`, `verification.jsonl`, the build receipts in
`benchmark/.cache/indexing/`, and any logs or indexes needed for auditing.
`RESULT.json` contains per-batch and combined paired statistics, and
`REPORT.md` gives the measured phase medians. Both can be regenerated from a
complete run:

```sh
python3 -B benchmark/run.py indexing summarize --output /path/to/completed-run
```
