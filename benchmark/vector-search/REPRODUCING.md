# Reproduce the published vector workloads

Run these commands from the repository root. The supported environment is macOS,
Rust **1.98.1**, a complete **JDK 23**, and Python **3.9+**. Neural token-vector
generation additionally uses an isolated Python 3.12 environment with the pinned
requirements below. Compiling drivers, generating inputs, constructing graphs,
checking compatibility, and timing queries are separate, explicit actions.

| Preset | Documents | Dimensions | Queries | Similarity | M / beam | RAM MiB |
|---|---:|---:|---:|---|---|---:|
| `sift1m` | 1,000,000 | 128 | 10,000 | Euclidean | 16 / 100 | 2048 |
| `glove-wiki-100` | 1,000,000 | 100 | 2,641 | Dot product | 16 / 100 | 2048 |
| `glove-wiki-300` | 1,000,000 | 300 | 2,641 | Dot product | 16 / 100 | 2048 |
| `minilm-wiki-384-m16-1m` | 1,000,000 | 384 | 2,641 | Dot product | 16 / 100 | 2048 |
| `minilm-wiki-384` | 500,000 | 384 | 2,641 | Dot product | 64 / 250 | 2048 |
| `mpnet-wiki-768` | 500,000 | 768 | 2,641 | Dot product | 64 / 250 | 2048 |

These presets reproduce the stated workloads and measurements, not a historical
machine's elapsed times or its executable identity. Drivers have been migrated
and parameterized; a new executable must have its own validation and receipt.
No large inputs, weights, indices, binaries, or historical result files are
committed. Cohere is not included in this first set of reproduction presets.

## Reproduce the measured Alpha.3 release

The six configurations above were fully measured at
`v0.1.0-alpha.3`, commit `9406d0fcd491b5d9d31d67b0cc01f9fa89775228`.
The release workflow freezes that exact source from local Git, the current
benchmark code, and the measured release-specific Cargo.lock. It does not switch
the checkout or benchmark whichever library happens to be on `main`.
`release.json` pins all six preset hashes and the measured Rust/Java drivers,
sampling implementation and metrics. The additive codec configuration patch is
still disclosed as `library_modified` in the frozen build receipt.

Inspect without compiling, downloading, locking or creating directories:

```sh
python3 -B benchmark/run.py vector-search release-plan
```

For a full reproduction, create the Python3.12 inference environment described
below, then choose persistent data and fresh result/index directories:

```sh
python3 -B benchmark/run.py vector-search release-run \
  --output "$PWD/benchmark/runs/vector-alpha3" \
  --index-root "$PWD/benchmark/indexing/runs/vector-alpha3" \
  --data-root /path/to/persistent/benchmark/data \
  --sources /path/to/persistent/benchmark/data/vector-sources \
  --java-home /path/to/jdk-23 \
  --inference-python /path/to/inference-venv/bin/python \
  --download
```

Only missing pinned sources are downloaded with `--download`. Exact existing
base/query vectors and valid truth are reused; no inference environment is needed
when all six datasets already exist. Missing Wiki truth is generated explicitly
as a recorded suite stage. The MiniLM500k input is the first500k rows of the
measured 1M input, with identical query bytes; its own truth is computed for500k.
No extra normalization or inference changes are introduced.

`release-build` accepts the same options and stops after compiling drivers,
without constructing a graph or timing queries. It leaves `STATE.json` as
`READY`; explicitly continue it with:

```sh
python3 -B benchmark/run.py vector-search release-resume \
  --output "$PWD/benchmark/runs/vector-alpha3"
```

Resume uses the saved plan and frozen harness, not later main edits. Completed
stages and earlier attempt logs are retained. Only a pending/failed preparation,
build or export stage can resume; a failed/interrupted full performance stage
is never replayed automatically or by `release-resume`. Inspect and preserve its
partial `RUN.json`; do not delete or replace observations to obtain better data.
Ctrl+C stops owned children and releases the lock; it never schedules a resume.

One exclusive performance lock covers source freezing, driver compilation, all
data/truth preparation, all six comparisons, final identity/compatibility/round
checks and export. The order matches the completed run: MPNet, SIFT, GloVe100,
GloVe300, MiniLM1M, MiniLM500k. New indices stay under `--index-root`; complete
raw evidence is also copied without indices to `--output/results/PRESET/`.
`FINAL_AUDIT.json` and `SUMMARY.md` are written only after all six complete.
Never remove shared data as part of routine output cleanup.

Each new run records its own paths, source/consumer hashes, actual crate
metadata, toolchain, JAR/JDK/classes/binaries and all commands. Different paths,
hardware or generated code mean new identities and timings; these commands
reproduce the measured workload and method, not the historical executable or
identical elapsed times. The build uses the dedicated Alpha.3 lock rather than
resolving dependencies from today's registry. The generic current-checkout build
keeps its separate existing lock. No website publication is performed.

The completed Alpha.3 run passed all24 index /48 reader-path structural checks.
SIFT's cross-engine graph, ordered IDs, f32 bits and visited matched strictly.
Wiki cross-writer graph digests differed and hot scores differed by at most1ULP,
with some ordering/visited differences; GloVe300 fanout0 had two different hit
sets. Average recall matched. Preserve these distinctions in new reports rather
than equating index compatibility with cross-engine score or graph equality.

## Inspect an individual plan without running anything

```sh
python3 -B benchmark/run.py vector-search presets
python3 -B benchmark/run.py vector-search plan --preset mpnet-wiki-768
```

`plan` reads configuration only. It does not download, compile, generate truth,
build an index, acquire the performance lock, or run a query.

## Compile the independent drivers

```sh
python3 -B benchmark/run.py vector-search build --preset mpnet-wiki-768 \
  --java-home /path/to/jdk-23
```

One build serves all six presets. `build` **compiles executables**, while
`build-index` measures index construction. Optional `--jars-dir` reuses the exact
JARs listed in `benchmark/common/java-jars.json`; otherwise those pinned artifacts
are downloaded. Drivers use published Lucene **10.1.0**, normal JIT/VectorAPI,
default GC and `-Xms512m -Xmx6g`. Rust is an independent Cargo consumer, outside the
library workspace, built with ordinary `--release --locked`; no native CPU, LTO,
PGO, or library test targets are enabled.

The source, consumer, and target paths are fixed under
`benchmark/.cache/vector-search-reproduction/`. Every build records the source
commit **and current bytes**, driver/lock/JAR/class/executable hashes, toolchain,
the actual library rustc command and crate metadata. It checks identities again
before and after each measurement. Rebuild explicitly after changing the library,
benchmark, JDK, or committed source identity. Existing executables are never
silently rebuilt by a query command.

### Codec configuration in the frozen source

The measured M64 runs required an additive `Lucene101Codec` vectors-format
builder that was present only in their frozen source. The new build stage copies
the checkout's source bytes into the fixed cache source directory and adds that
same configuration entry **only if missing**. The checkout is unchanged. The
resulting diff, patch hash, and complete frozen source identity are saved in the
build receipt; `library_modified` explicitly says whether the patch was applied.
The driver then uses `Lucene99HnswVectorsFormat::with_graph_para(M, beam)` through
the ordinary codec path. There is no HNSW, scoring, flush-budget, or similarity
algorithm patch. Do not describe a patched build as an unmodified commit.

Each build attempt preserves its logs, receipt and previous source/consumer/class
snapshot. Changing the build location can change Rust metadata and generated
code; compare performance only with the recorded identities and paths in view.

## Prepare or reuse data

An already generated directory may be supplied using `--data`. Its raw file sizes
and SHA256 values must match the preset, or a new input identity must be selected
explicitly as described below. A download is never implied by a query run.

For SIFT1M, either reuse a pinned archive under `--sources/sift.tar.gz`, or:

```sh
python3 -B benchmark/run.py vector-search prepare-data --preset sift1m --download
```

This uses the existing pinned SIFT mirror and removes each fvecs/ivecs row header
without changing the payload. The published exact groundtruth is included in the
archive; a new exhaustive truth run is unnecessary.

For Wiki presets, `--sources` contains the paths in
[`datasets/vector-wiki/manifest.json`](../datasets/vector-wiki/manifest.json):
`downloads/` for original archives and `models/MODEL/` for pinned model files.
Missing source files are downloaded only with explicit `--download`.

```sh
python3 -B benchmark/run.py vector-search prepare-data --preset glove-wiki-100 \
  --sources /path/to/vector-sources --download
```

Use `glove-wiki-300` to generate the 300D data. This reads the **original Stanford
archive** selected by luceneutil, not a similarly named archive from a mirror.
All sources are verified against the recorded complete-file hashes.

For MiniLM/MPNet, prepare an isolated environment explicitly:

```sh
python3.12 -m venv /path/to/inference-venv
/path/to/inference-venv/bin/python -m pip install \
  -r benchmark/datasets/vector-wiki/requirements-inference.txt
python3 -B benchmark/run.py vector-search prepare-data --preset mpnet-wiki-768 \
  --sources /path/to/vector-sources --download \
  --inference-python /path/to/inference-venv/bin/python
```

Generation verifies `pip freeze --all`, the complete model file set, revisions,
and the pinned upstream scripts. It preserves upstream tokenization, first
`>400000` unique-token boundary, count `>1`, token insertion order, and batch256
model encoding. The input dictionary prefix includes the original header, as in
upstream inference; the selected **document text** skips that header. The first
N documents and all **2,641 pinned Wiki queries** are kept in order, including
duplicates and zero-vector queries.

The generated vectors use upstream `WikiVectors` / `VectorDictionary` token
aggregation and normalization. This is not whole-document SentenceTransformer
encoding. There is no extra normalization, zero-vector deletion, deduplication,
dimension padding, or truncation. Preparation saves source and output identities,
commands, inference environment and all errors under the data directory.

Model-generated float bits can differ on another runtime/device. A mismatch is
reported and not silently accepted. After inspecting the new bytes, repeat the
identity check with `--accept-new-inputs` in a fresh output directory, or provide
the generated raw files directly to `truth` with that option. Every following
command must explicitly select `--accept-new-inputs`. Such a run has a new dataset
identity and is not labelled `standard_full_run`. Never pair new vector bytes
with stale truth. Failed or partial outputs are retained and never overwritten.

## Generate graph-independent groundtruth

For each Wiki preset:

```sh
python3 -B benchmark/run.py vector-search truth --preset mpnet-wiki-768 \
  --data /path/to/mpnet-data
```

This is an explicit, potentially expensive exhaustive `documents × queries` scan
with published Java10.1 scoring after at least60s actual scoring warmup. Three
queries also use independent full sorting to check the Top100 heap. The receipt
binds truth to the exact vector bytes, dimensions, similarity and row counts.
Zero vectors, norm statistics, score bits and deterministic tie-breaking are
recorded. It is independent of the HNSW graph. Existing truth is never overwritten.

## Full comparison or individual stages

```sh
# Four new indices: Rust1 / Java1 / Java2 / Rust2, then compatibility and queries.
python3 -B benchmark/run.py vector-search all --preset mpnet-wiki-768 \
  --data /path/to/mpnet-data --output /path/to/new-full-run

# Alternatively, measure construction only.
python3 -B benchmark/run.py vector-search build-index --preset mpnet-wiki-768 \
  --data /path/to/mpnet-data --output /path/to/new-build-run

# Reuse completed indices; no index is constructed implicitly.
python3 -B benchmark/run.py vector-search query --preset mpnet-wiki-768 \
  --data /path/to/mpnet-data --index-run /path/to/new-build-run \
  --output /path/to/new-query-run
python3 -B benchmark/run.py vector-search verify-index --preset mpnet-wiki-768 \
  --data /path/to/mpnet-data --index-run /path/to/new-build-run \
  --output /path/to/new-verification-run
```

`query --index /path/to/external-index` also supports a read-only existing index.
Its construction parameters are unverified, and it is not a standard full run.
`--index-run` validates the preset, original vectors and index file hashes against
its retained construction receipt. Standalone query reports leave unmeasured
construction fields as **N/A**, rather than borrowing another run's times.

Each heavy action holds `/tmp/rlucene-performance-build.lock` until child cleanup
and final reporting complete. Builds, input generation, exhaustive truth, graph
construction, compatibility and query measurement are serialized. No CPU-load
gate, automatic timing retry, sample deletion, or favourable-wave selection is
used. Ctrl+C stops this run's children, preserves partial files and marks
`STOPPED_BY_USER`; it never resumes automatically.

## Correctness, metrics and retained evidence

* Construction records input/decode/document/add loop, commit, merge waiting,
  initial close, APPEND writer forceMerge lifecycle, complete total, vectors/s,
  RSS, actual index bytes and segment count. Configuration is printed and checked.
* Full verification reads every completed Rust/Java index with **both readers**,
  checks all original vector bits, the stored-ID permutation and doc-ord mapping,
  same-index canonical graph digest and integrity, then compares complete-query
  and independent leaf/visited paths. Graph differences between writers are
  reported; random segment metadata SHA equality is not required.
* Query measurement uses the same index (`b1-java` for receipt-backed runs), two
  fresh process batches, each engine's >=60s actual full-query warmup and12
  balanced AB/BA rounds. `fanout0` and `fanout100` are separate; search K is
  `100+fanout`, returning Top100. Throughput uses complete `RUN` elapsed time;
  P50/P95/P99 use separate `LATENCY` passes.
* SIFT uses strict ordered-ID/f32/visited equality. Wiki uses `report`: every
  ordering, hit-set, same-document score-bit and visited difference is preserved
  in per-query CSV/JSON. `--score-policy strict` can require equality. **Structural
  index verification always remains strict**, regardless of score policy.
* Reports include thread CPU and process CPU separately, invalid thread snapshots,
  average cores, process-exit RSS, Recall@100 and independent visited. CPU-invalid
  rounds retain their wall-clock samples. Payload RAM estimates are not heap/RSS.

`RUN.json` is the execution/identity receipt. `raw/` holds protocols, stderr,
results, per-query timings, CPU snapshots, graph bytes and mappings.
`differences/` retains complete per-query differences. `RESULT.json`, `REPORT.md`,
`LUCENEUTIL_METRICS.csv`, `per-query.csv` and `REGRESSIONS.json` summarize the
observations. Partial paired rounds are retained in `RUN.json`; reporting uses
only completed pairs and labels incomplete batches explicitly.

To summarize again without executing any benchmark:

```sh
python3 -B benchmark/run.py vector-search report --output /path/to/existing-run
```

## Relationship to luceneutil and the original runner

The fixed reference is luceneutil
[`c7eeab0`](https://github.com/mikemccand/luceneutil/tree/c7eeab0ceb661dae8cf4171ceed7761689acc0e0).
Its current preset uses500k documents, M64/beam250,10k queries, fanout100,
RAM128MiB and parallel/quantized/rerank grids. The table above records the actual
published serial nonquantized comparisons: RAM2048,2,641 Wiki queries, and the
older GloVe/SIFT M16/beam100 document counts. These are adaptations, not the full
unmodified upstream grid. See [METRICS.md](METRICS.md) for precise boundaries.

The original commands without `--preset` remain the query-only SIFT v1 protocol.
They use `configs/sift1m-top100.json`, their existing cache/receipts, and stricter
historical input identities. Their `run` timing includes per-request timers;
do not merge those observations with the new separate RUN/LATENCY protocol.
The new `reproduction/` cache and result directories never overwrite v1 evidence.
