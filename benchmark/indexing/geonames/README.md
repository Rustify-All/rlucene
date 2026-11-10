# GeoNames million-document indexing benchmark

The [published benchmark](https://benchmark.amazingkoala.com.cn/indexing.html)
uses the GeoNames worldwide dump dated 2026-11-10. Its archive and derived input
checksums are in the [dataset manifest](../../datasets/geonames/manifest.json).
[GeoNames](https://www.geonames.org/) publishes the data under
[CC BY 4.0](https://creativecommons.org/licenses/by/4.0/). The
[official download](https://download.geonames.org/export/dump/allCountries.zip)
is refreshed daily. Use a saved archive matching the manifest to reproduce
this report.

From the repository root on macOS, use Python 3.9+, Rust 1.98.1 via rustup and
JDK 23. Set `JAVA_HOME` or pass `--java-home` to select the JDK.

```sh
python3 -B benchmark/run.py indexing smoke --dataset geonames-1m
python3 -B benchmark/datasets/prepare.py geonames-1m --archive /path/to/allCountries.zip
rustup component add llvm-tools-preview --toolchain 1.98.1
python3 -B benchmark/run.py indexing run --dataset geonames-1m
```

The smoke command builds both drivers and verifies the 13-row boundary fixture.
It needs no GeoNames archive. The full run uses
[preset geonames-1m](../presets/geonames-1m.json), including the measured RLucene
commit, and follows the [three-engine protocol](../README.md#geonames-and-nyc-yellow-taxi-on-macos).
The data preparer selects zero-based row `floor(i × N / 1,000,000)` for each
`i`, preserving source order. It derives the input and the independent queries
and checks their bytes against the published sample.

Both writers reuse field slots. They store the canonical GeoNames ID as a
StringField and index coordinates with LatLonPoint. Population uses a LongPoint
and NumericDocValues. Empty populations are omitted and zero is retained.
Aliases are split on commas and passed to SortedSet DocValues in original
order, with empty items removed. The reader audit verifies the resulting
sorted, deduplicated values. Query checks cover exact IDs, geographic bounds
including the antimeridian, radius, population filtering and sorting, and aliases.

Use `--destination DIR` for preparation and `--data DIR` for the run to reuse a
shared data directory. Use `--jars-dir` to reuse the pinned JARs locally and
`--output` to choose a new run directory. The default data and output directories
are `benchmark/data/geonames-1m` and `benchmark/runs/geonames`.
