#!/usr/bin/env python3
"""Run a benchmark suite: python3 benchmark/run.py SUITE ACTION [OPTIONS]."""
import os
from pathlib import Path
import sys

ROOT = Path(__file__).resolve().parent
SUITES = {"search-benchmark-game": "Wikipedia / five search modes",
          "vector-search": "SIFT1M + Wiki 100/300/384/768D / HNSW build, query, compatibility",
          "indexing": "Indexing / Rally HTTP logs, MS MARCO passages, GeoNames, NYC Yellow Taxi and Wikipedia medium"}

if len(sys.argv) < 2 or sys.argv[1] in {"list", "-h", "--help"}:
    print(__doc__)
    for name, description in SUITES.items():
        print("  %-24s %s" % (name, description))
    sys.exit(0)
suite = sys.argv[1]
if suite not in SUITES:
    sys.exit("Unknown suite. Run: python3 benchmark/run.py list")
entry = ROOT / suite / "bench.py"
if not entry.is_file():
    sys.exit("This method is in preparation. See " + str(ROOT / suite / "README.md"))
os.execv(sys.executable, [sys.executable, "-B", str(entry), *sys.argv[2:]])
