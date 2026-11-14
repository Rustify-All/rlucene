# Vector search benchmarks

For the six measured SIFT/Wiki configurations, complete graph construction, query
measurement and bidirectional index verification, start with
**[REPRODUCING.md](REPRODUCING.md)**. It documents explicit stages, source/data
identities, groundtruth generation, result differences and partial-run handling.

`release-plan`, `release-build` and `release-run` select the measured Alpha.3
source and dedicated dependency lock. The release guide covers both MiniLM
document counts, all six serial comparisons and explicit preparation-only resume.

```sh
python3 -B benchmark/run.py vector-search presets
python3 -B benchmark/run.py vector-search plan --preset mpnet-wiki-768
```

The section below preserves the original **query-only SIFT v1** commands and
measurement protocol. New preset runs use a separate cache and receipt format.

## Original SIFT1M Top100 query protocol

This is an independent Lucene 10.1 / RLucene comparison adapted from
[luceneutil](https://github.com/mikemccand/luceneutil/tree/f77ba1c41c181b9361a7ab5d48fa83b4a367f2e9).
It reuses the previously validated standalone `SiftBench` and Rust query drivers.
It does not claim to be unmodified luceneutil output.

## Prepare or reuse inputs

```sh
python3 -B benchmark/datasets/prepare.py sift1m
python3 -B benchmark/run.py vector-search build
# Explicit setup action only; regular query runs reuse an index read-only:
python3 -B benchmark/run.py vector-search prepare-index
```

The dataset is the [TexMex SIFT1M corpus](http://corpus-texmex.irisa.fr/), downloaded
from the pinned mirror and verified against the original recorded archive and
individual-file SHA-256 values in `datasets/sift1m/manifest.json`. Conversion
removes each fvecs/ivecs row's dimension header, retaining the original
little-endian payload without normalization. Python standard library only.

An existing dataset directory can be used through `--data`; query execution
requires `query.raw` and `groundtruth.raw`. Setup additionally requires `base.raw`.
Use `--index` for an existing one-segment Lucene101 index with one million vectors
and stored `id` equal to internal docID. The pinned historical index manifest is
included; the runner records whether the supplied index matches it. Rebuilding
with the standalone Java setup command records a new index identity and setup
receipt. An index matching neither the published manifest nor its setup receipt
has unverified build parameters and is labelled `standard_full_run=false`.

## Verify and run

```sh
python3 -B benchmark/run.py vector-search smoke --index /path/to/index --data /path/to/data
python3 -B benchmark/run.py vector-search run --index /path/to/index --data /path/to/data
```

Here `smoke` is a correctness-only full sweep, not a few-vector sample: 10,000
normal queries plus 10,000 independent leaf/visited queries per engine. It checks
all ordered docIDs/f32 bits, verifies the two paths agree, verifies visited counts
across engines, and computes Recall@100 against groundtruth. It performs no warmup
or formal performance sampling. `run` includes the same correctness phase.

The versioned configuration is `configs/sift1m-top100.json`: 1M x 128D float32,
Euclidean, Top100, M=16, beamWidth=100, seed=42, one segment, no filters or
quantization. Java uses JDK23 normal JIT with vector/native access and the original
512 MiB initial / 6 GiB maximum heap, default GC.

Two fresh-process batches, >=60 seconds internal mixed-query warmup per engine,
then 12 complete 10,000-query sweeps. Full sweeps alternate AB/BA, six of each order
per batch. Each sweep writes all 10,000 raw request nanoseconds plus elapsed and
process CPU counters. All per-query regressions, partial observations, protocol
responses, full hit/visited outputs and process-exit RSS are retained.

See [METRICS.md](METRICS.md) for timing boundaries and the native versus extended
metrics. Query runs do not measure graph building, writing, merge or commit.
`prepare-index` is an explicit setup action, not a graph-build performance result.
The next indexing method must define and verify those measurements separately.
