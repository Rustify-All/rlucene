# Wikipedia medium · latest release

This method compares the latest recorded RLucene release with Java Lucene 10.1.0.
[release.json](../release.json) selects the exact release. The [main guide](../README.md)
covers environment setup and all five datasets.

## Input and execution

The input is the first 1,000,000 UTF-8 rows after the header of the pinned
luceneutil Wikipedia archive, preserving source order and field values.
The [manifest](../../datasets/wiki-medium-indexing/manifest.json) records the
archive and corpus checksums.

```sh
python3 -B benchmark/datasets/prepare.py wiki-medium-indexing-1m
python3 -B benchmark/run.py indexing release-smoke --dataset wiki-medium-indexing-1m
python3 -B benchmark/run.py indexing release-run --dataset wiki-medium-indexing-1m
```

Use `--archive PATH` for preparation and
`--data wiki-medium-indexing-1m=DIR` to reuse a directory with `corpus.txt`.
`--output DIR` selects a new result directory. `--java-home` and `--jars-dir`
select a local JDK and pinned JARs.

The runner freezes the release from Git without switching the checkout. It
builds **one native-CPU/fat-LTO Rust consumer, without PGO**, and Java. Smoke
verifies 4,096 rows per engine; the full run measures the million-document
protocol. No older Rust version is built.

## Workload and verification

Both writers reuse a document and eight field slots. `titleTokenized` and `body`
use StandardAnalyzer with frequencies, positions and norms. Exact title, random
label, ID and date have postings. Tokenized title, ID and date are stored; the
body is not. ID and seconds-from-midnight have IntPoint indexes. There are no
DocValues or term vectors.

The preset uses one writer, explicit MMapDirectory, a 2 GiB buffer,
TieredMergePolicy, one concurrent merge thread and no compound files. Each
verified output has one segment. No explicit forceMerge is performed.

Write covers ingestion, parsing, assignment, analysis and insertion. Commit
and close are separate. Total spans timed writer release through close,
including timer gaps. Rust's UTF-16 statistic is checked outside timing.

Each of two batches starts fresh Java and Rust processes. Each process completes
at least 60 seconds of full-index warmup before three formal writes. The batches
balance engine order and produce six formal pairs. Every preflight, warmup and
formal index passes Java CheckIndex, a logical-content digest and Rust reopen
outside timing. All observations and indexes are retained.

The latest saved observations can be recomputed without datasets or builds:

```sh
python3 -B benchmark/run.py indexing release-reference
```
