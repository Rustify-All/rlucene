# Latest release indexing benchmark

This directory reproduces the five million-document comparisons for the latest
recorded release: **RLucene 0.1.0-alpha.3**, commit
`9406d0fcd491b5d9d31d67b0cc01f9fa89775228`, against Apache Lucene 10.1.0.
[release.json](release.json) selects the exact release. The library is extracted
from Git without switching your checkout or creating a worktree. Benchmark code,
consumers and build identities are frozen in each new output directory.

## Review the saved result without a benchmark run

From the repository root, with Python 3.9+:

```sh
python3 -B benchmark/run.py indexing release-reference
```

This recomputes every saved statistic from all 78 formal groups in
[reference/latest.json](reference/latest.json), checks input-manifest hashes and
prints the table below. It requires no datasets, Rust, Java, NumPy or PyArrow,
and performs no builds, downloads or index writes.

| Dataset | Ordinary Rust throughput vs Java | Optimized Rust throughput vs Java |
|---|---:|---:|
| Rally HTTP Logs | +7.140% | +28.595% |
| MS MARCO Passage | +19.679% | +42.464% |
| GeoNames | +9.786% | +29.330% |
| NYC Yellow Taxi | +16.705% | +44.536% |
| Wikipedia medium | Not measured | +30.649% |

Throughput change is `100 × (median(Java total) / median(Rust total) − 1)`.
The reference preserves phase timings, paired medians, paired geometric means,
every formal observation, build fingerprints and validation counts.
The first four optimized builds use **native CPU + ThinLTO + same-input PGO**;
Wiki uses **native CPU + fat LTO, without PGO**. Optimized results are not
ordinary-release results. New machines and build paths produce their own times
and fingerprints.

## Environment

The supported runner is macOS. The reference used macOS 27.2.1 arm64, 12 CPUs,
Python 3.9.6, Rust 1.98.1 and JDK 23.0.2. Use a full Git clone containing the
tag in `release.json`.

```sh
rustup toolchain install 1.98.1 --profile minimal
rustup component add llvm-tools-preview --toolchain 1.98.1
python3 -m venv benchmark/.cache/indexing-python
. benchmark/.cache/indexing-python/bin/activate
python -m pip install -r benchmark/datasets/nyc-yellow-taxi/requirements.txt
```

Taxi requires NumPy 2.0.2 and PyArrow 21.0.0. The launcher checks them before
building. Set `JAVA_HOME` or pass `--java-home /path/to/jdk-23.0.2`.
`--jars-dir DIR` reuses local pinned Java JARs; their SHA-256 is checked.
Allow space for archives, prepared data, build caches, profiles, preflight
indexes and retained Wikipedia indexes.

## Prepare the fixed inputs

```sh
python -B benchmark/datasets/prepare.py rally-http-logs-1m
python -B benchmark/datasets/prepare.py ms-marco-passage-1m
python -B benchmark/datasets/prepare.py geonames-1m
python -B benchmark/datasets/prepare.py nyc-yellow-taxi-2025-01-1m
python -B benchmark/datasets/prepare.py wiki-medium-indexing-1m
```

The default directories are `benchmark/data/DATASET`. Manifests pin the source,
sampling rules, prepared files and checksums. See [Rally](../datasets/rally-http-logs/README.md),
[MS MARCO](../datasets/ms-marco-passage/README.md), [GeoNames](geonames/README.md),
[Taxi](nyc-yellow-taxi/README.md) and [Wikipedia](wiki-medium/README.md).

GeoNames requires the recorded **2026-11-10** archive. Its official URL serves
a daily export, not an immutable dated file. If a download differs from the
pinned SHA-256, supply the retained archive with the preparer's `--archive`
option. Do not substitute a different snapshot. The saved reference samples
still allow statistical review without the source archives.

## Inspect, validate and measure

```sh
python -B benchmark/run.py indexing release-plan
python -B benchmark/run.py indexing release-smoke --output /path/to/new-smoke
python -B benchmark/run.py indexing release-run --output /path/to/new-run
python -B benchmark/run.py indexing release-report --output /path/to/new-run
```

- `release-plan` prints the release, input locations and exact presets only.
- `release-smoke` builds drivers and runs the original preflights. It produces
  no performance result. Rally checks the full million rows; MS MARCO checks
  its fixture and small sample; GeoNames/Taxi check fixtures; Wiki checks 4,096
  rows after verifying its fixed corpus.
- `release-run` builds, trains, validates, warms up and measures the full
  protocol, then creates the combined summary.
- `release-report` recomputes a completed run's table from its raw formal
  samples without executing Rust or Java.

Select one dataset with `--dataset geonames-1m`. For another input layout, use
`--data-root DIR` or repeat `--data DATASET=DIR`:

```sh
python -B benchmark/run.py indexing release-run \
  --dataset wiki-medium-indexing-1m \
  --data wiki-medium-indexing-1m=/path/to/prepared-wiki \
  --output /path/to/new-wiki-run
```

Outputs must be new. Failed/stopped runs remain in place; there is no automatic
retry or resume. The launcher holds `/tmp/rlucene-performance-build.lock` across
staging, builds, training, validation and measurement; child runners inherit
the same lock. CPU load never gates, filters or replaces samples.

## Protocol and timing

| Setting | First four workloads | Wikipedia medium |
|---|---|---|
| Documents / writers | 1,000,000 / one | 1,000,000 / one |
| Directory / compound files | Explicit mmap / disabled | Explicit mmap / disabled |
| Buffer / document flush threshold | 256 MiB / 65,536 | 2,048 MiB / disabled |
| Merge policy / final segments | NoMerge / 16 | TieredMergePolicy / one |
| Formal process batches | Three | Two |
| Same-process warmup | Two complete writes per engine per batch | At least 60 seconds per engine per batch |
| Formal groups | Six three-engine orders per batch, 18 total | Three balanced pairs per batch, six total |
| Rust PGO | Four writes of the same million-row input | None |

Java uses normal JIT, a fixed 4 GiB heap and ParallelGC. Rally enables the Vector
API. Wiki also uses the explicit C2, SIMD/FMA and heap pre-touch options in its
preset. Rust consumers have their own `[workspace]` and Cargo.lock; library
unit-test targets are not used. PGO training, warmup and verification are outside
formal timing.

Write includes field updates, analysis where applicable, insertion and automatic
flushes. Rally/MS MARCO/Taxi decode preloaded binary rows inside the loop;
GeoNames decodes before timing. Wiki includes line ingestion, queue waiting and
parsing, with its bounded reader started before release. Commit and close are
separate. Wiki total spans timed writer release through close, including timer
gaps; other totals sum the three phases. Setup and directory close are excluded.

Preflight performs the dataset's CheckIndex, content/points/DocValues/postings
audits and independent queries. Timed indexes are reopened after each batch;
GeoNames/Taxi use both Java and ordinary Rust readers. Verified structured/passage
indexes are removed by the original runners; raw observations remain. Wiki
checks every index with CheckIndex, a full logical digest and Rust reopen.

## Evidence and updates

Each output keeps `RELEASE_RUN.json`, `CONTEXT.json`, `LIBRARY.json`, the source
archive, frozen harness, `STAGING.patch`, build cache and per-dataset directories.
Dataset directories retain `RUN.json`, `samples.jsonl`, `verification.jsonl`,
build/PGO receipts, toolchain/JVM/JAR identities and logs. `SUMMARY.json` and
`SUMMARY.md` are derived reports. Preserve all phase regressions.

For a newer release, update `release.json`, the source pins in the presets and
`reference/latest.json` from verified completed measurements. The public method
and reference keep only the latest release. These tools do not publish the
website or change the library checkout.
