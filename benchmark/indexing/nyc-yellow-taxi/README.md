# NYC Yellow Taxi million-document indexing benchmark

The [published benchmark](https://benchmark.amazingkoala.com.cn/indexing.html)
uses NYC TLC's January 2025 Yellow Taxi trip records. See the
[official trip records page](https://www.nyc.gov/site/tlc/about/tlc-trip-record-data.page),
[field dictionary](https://www.nyc.gov/assets/tlc/downloads/pdf/data_dictionary_trip_records_yellow.pdf)
and [dataset manifest](../../datasets/nyc-yellow-taxi/manifest.json).

From the repository root on macOS, use Python 3.9 (verified with the versions below), Rust 1.98.1 via rustup and
JDK 23. Set `JAVA_HOME` or pass `--java-home` to select the JDK. Install the
pinned Parquet reader in a separate Python environment.

```sh
python3 -m venv benchmark/.cache/nyc-taxi-venv
. benchmark/.cache/nyc-taxi-venv/bin/activate
python -m pip install -r benchmark/datasets/nyc-yellow-taxi/requirements.txt
python -B benchmark/run.py indexing smoke --dataset nyc-yellow-taxi-2025-01-1m
python -B benchmark/datasets/prepare.py nyc-yellow-taxi-2025-01-1m
rustup component add llvm-tools-preview --toolchain 1.98.1
python -B benchmark/run.py indexing run --dataset nyc-yellow-taxi-2025-01-1m
```

The smoke command builds both drivers and verifies the 16-row boundary fixture
without the Parquet file. The full run uses
[preset nyc-yellow-taxi-2025-01-1m](../presets/nyc-yellow-taxi-2025-01-1m.json),
including the measured RLucene commit, and follows the
[three-engine protocol](../README.md#geonames-and-nyc-yellow-taxi-on-macos).

The preparer verifies the original Parquet SHA-256 and selects zero-based row
`floor(i × 3,475,226 / 1,000,000)` for each `i` in source order. The original
row number is the external ID, so duplicate rows retain separate IDs. The two
timestamps retain their physical microsecond values. Timestamp, zone, distance
and fare fields use matching point and DocValues fields. Nulls omit both
fields, DoublePoint canonicalizes NaNs and DocValues retain their original bits.
The query oracle checks exact IDs, ranges, conjunctions, field presence and
numeric ordering. The preparer verifies the bytes of both engines' common
inputs and all derived queries against the published sample.

Pass `--archive /path/to/yellow_tripdata_2025-01.parquet` to reuse an existing
file. Use `--destination DIR` for preparation and `--data DIR` for the run to
reuse a shared directory. Use `--jars-dir` to reuse the pinned JARs locally
and `--output` to choose a new run directory. The default data and output
directories are `benchmark/data/nyc-yellow-taxi-2025-01-1m` and
`benchmark/runs/nyc-yellow-taxi`.
