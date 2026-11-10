# Wikipedia medium · one million documents

This preset uses the first 1,000,000 rows after the header in the English
Wikipedia line-document archive used by luceneutil. It preserves source order
and verifies the archive and prepared UTF-8 corpus against the
[input manifest](../../datasets/wiki-medium-indexing/manifest.json).
The [Java drivers](driver/java/) and [Rust driver](driver/src/main.rs)
are byte-identical to those used for the recorded version comparison.

## Reproduce

Use macOS arm64, Python 3.9+, Rust 1.98.1, JDK 23.0.2 and a Git clone containing
both RLucene revisions in the [preset](../presets/wiki-medium-indexing-1m.json).
From the repository root:

```sh
python3 -B benchmark/datasets/prepare.py wiki-medium-indexing-1m
python3 -B benchmark/run.py indexing smoke --dataset wiki-medium-indexing-1m
python3 -B benchmark/run.py indexing run --dataset wiki-medium-indexing-1m
```

Use `--archive /path/to/enwiki-20120502-lines-1k-fixed-utf8-with-random-label.txt.lzma`
with the preparation command to reuse the pinned archive. Use `--data` with the
runner to select an existing directory containing the exact `corpus.txt`.
`--java-home` and `--jars-dir` select a local JDK and pinned JARs.
`--output` selects a fresh run directory. Allow 20 GiB of free space for the
archive, corpus, build and retained indexes.

`smoke` builds both Rust revisions and the Java drivers, then writes and verifies
a 4,096-document index per engine. `run` performs that preflight and the full
million-document protocol. The runner archives both pinned revisions without
switching the checkout. It compiles the same external Rust consumer at the
same source, consumer and target paths, using native CPU and fat LTO. It
uses the JDK flags in the preset and Java Lucene 10.1.0. This preset has no
PGO training step.

## Workload and output

Both writers reuse one document and its fields and use StandardAnalyzer for
the body and tokenized title. The exact title, date, ID, label and points are
fixed by the drivers. The preset uses one writer thread, MMapDirectory, a 2 GiB RAM
buffer, one concurrent merge thread, and no compound files. Each completed
index has one segment.

Write time covers input reading, parsing, field assignment, analysis and
document insertion. Explicit commit and writer close are measured separately.
Full time runs from the timed release of the writer thread through close,
including the small gaps between phase timers. Rust's UTF-16 length statistic is checked
outside timing.

Each of two batches starts three fresh processes. Every process performs at
least 60 seconds of complete writes before timing. The two batches together
cover all six execution orders. Every preflight, warmup and formal index is
checked with Java CheckIndex, a logical content scan and a Rust reopen. All
six formal triplets and all validation receipts are retained. Compilation,
validation and timing use the shared performance lock.

`RUN.json` records the inputs, revisions and build fingerprints.
`samples.jsonl` and `verification.jsonl` preserve each round and audit.
`RESULT.json` contains per-engine medians and same-round elapsed changes.
Compute Rust's throughput advantage from the full medians as
`100 × (Java total median / Rust total median − 1)`.
