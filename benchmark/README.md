# Reproduce RLucene benchmarks

This directory contains the public benchmark drivers and protocols behind the
[benchmark collection](https://benchmark.amazingkoala.com.cn/). Suites follow the
homepage cards. Official runner support is **macOS**; Windows ports can adapt the
small Python runners while preserving the inputs, protocol and measurement scope.

| Homepage card | Directory | Status |
|---|---|---|
| Search Benchmark, the Game | [search-benchmark-game](search-benchmark-game/README.md) | Wikipedia, 962 queries, five modes |
| Vector Search | [vector-search](vector-search/README.md) | SIFT1M, GloVe100/300, MiniLM384, MPNet768: HNSW build, query and compatibility |
| Indexing Benchmark | [indexing](indexing/README.md) | Rally HTTP logs, MS MARCO passages, GeoNames, NYC Yellow Taxi and Wikipedia medium, one million documents each |

See also [中文说明](README.zh-CN.md).

For the Rally indexing preset, download the [official source shard](https://rally-tracks.elastic.co/http_logs/documents-181998.json.bz2),
or use the automatic download in the [dataset preparation guide](datasets/rally-http-logs/README.md).

## Requirements

- macOS, Git, Python 3.9+, Rust **1.98.1** via rustup. NYC Yellow Taxi also uses pinned NumPy and PyArrow.
- A complete **JDK 23**. Set `JAVA_HOME`, or use `--java-home`; otherwise runners
  use `/usr/libexec/java_home -v 23`. Published reference runs used JDK 23.0.2.
- Sufficient memory for the selected suite: full-text Java uses a 4 GiB heap;
  vector Java uses a 512 MiB initial / 6 GiB maximum heap.
- Model generation additionally requires the pinned Python3.12 inference environment.

See the [multi-dataset guide](vector-search/REPRODUCING.md) for all five presets,
pinned sources, explicit stages, and the recorded frozen codec configuration patch.
`vector-search presets` and `vector-search plan --preset mpnet-wiki-768` only inspect
configuration; they do not execute a benchmark.

```sh
rustup toolchain install 1.98.1 --profile minimal
python3 -B benchmark/run.py list
python3 -B benchmark/run.py indexing datasets
python3 -B benchmark/run.py search-benchmark-game build
python3 -B benchmark/run.py search-benchmark-game smoke --index /path/to/wiki-index
python3 -B benchmark/run.py search-benchmark-game run --index /path/to/wiki-index
```

`build` downloads official Java JARs from the pinned Maven URLs and verifies their
SHA-256, then builds an ordinary release downstream Rust consumer and a standalone
Java main. Each consumer has its own `[workspace]`, tracked `Cargo.lock` and a
`path` dependency on this checkout. No library test target is used.

Each suite documents data preparation and index construction. Existing indexes
can be reused read-only through `--index`. New indexes are created only by the
explicit index-building actions. The indexing suite creates fresh indexes for
its `smoke` and `run` actions.
Downloads, indexes and large outputs stay in
ignored `data/`, `.cache/` and `runs/` directories, or user-selected paths.

## Results and reproducibility

Each run records the library commit **and actual source hashes**, driver/config
hashes, toolchain/JVM/binary/JAR identities, machine/architecture, input/index
hashes, exact commands, warmup, order and raw observations. A changed source,
driver or JDK requires a new build. A modified library checkout is labelled with
its modified files and hashes rather than represented as the clean commit.

The default full-text protocol is two fresh-process batches, at least 60 seconds
of actual mixed-request warmup per engine/mode/batch, then 12 balanced AB/BA
rounds over all queries. New vector presets use complete sweeps:10,000 queries
for SIFT and all2,641 pinned queries for Wiki. The original SIFT commands without
`--preset` remain protocolv1; new presets use protocolv2. Their timing boundaries and CPU/RSS metrics are documented separately. CPU load is
never used to gate runs, remove samples or trigger retries.

Runs first compare independent complete correctness output. All regressions and
partial/stopped runs are preserved. Ctrl+C stops the active run; there is no
automatic resume. `summarize --output RUN_DIRECTORY` only reads existing data.
New vector presets use `report --output RUN_DIRECTORY` for read-only reporting.
Wiki score, ordering and visited differences are fully reported. Original SIFT
query checks and all structural index checks remain strict.

`RUN.json`, `RESULT.json`, `REGRESSIONS.json`, `per-query.csv` and raw files are
the reproducibility artifacts. Keep the build receipt/logs under `.cache/SUITE/`
with any shared result. Historical website reports retain their original commit,
index identity and driver version; new builds/paths/machines produce new results.
The website remains a separate presentation/import workflow; these scripts do
not publish reports.

## Add a method

Add a directory matching the method/card, with a README, upstream repository and
fixed revision, input provenance/checksums, Rust/Java public-API drivers, tracked
locks, a versioned protocol/config, independent correctness, timing definitions,
and raw-result analysis. Register its entry in `run.py`. Reuse `common/runtime.py`
for paths, ordinary builds, the shared performance lock and process I/O; keep
method-specific metrics in the suite. Mark incomplete methods as in preparation.

New benchmark code follows the repository Apache-2.0 license. The pinned query
set and original corpus transform have the upstream MIT license reproduced in
[UPSTREAM-LICENSE](search-benchmark-game/UPSTREAM-LICENSE).
