#!/usr/bin/env python3
"""Freeze an original-order, uniformly spaced million-row sample and its query oracle."""
import bz2
from collections import Counter
from datetime import datetime, timezone
import hashlib
import ipaddress
import json
from pathlib import Path
import tempfile
import struct

from common import runtime as rt

MANIFEST = Path(__file__).resolve().parent / "rally-http-logs/manifest.json"
TOTAL = 2708746
N = 1000000
START = 893980800000
END = 894067200000
OFFICIAL_CURSOR = 897436800000
TERM = "GET / HTTP/1.0"
HEADER = struct.Struct(">IqiI4sHH")


def save(path, payload):
    path.write_text(json.dumps(payload, indent=2) + "\n")


def ordered(pairs, reverse):
    return sorted(pairs, key=lambda pair: (-pair[0] if reverse else pair[0], pair[1]))


def transform(source, data, provenance):
    compressed_sha = provenance["archive"]["sha256"]
    binary = data / "million.bin"
    rows = data / "million.tsv"
    pos = 0
    expected_row = 1
    statuses = Counter()
    size_min, size_max = 2**31 - 1, 0
    seen_ts = Counter()
    by_case = {name: [] for name in ("term", "range200", "range400", "range")}
    docs = []
    omitted = 0
    sampled_sources = hashlib.sha256()
    decompressed = hashlib.sha256()
    with bz2.open(source, "rb") as stream, binary.open("wb") as wire, rows.open("wb") as out:
        for row, line in enumerate(stream, 1):
            decompressed.update(line)
            if pos >= N or row != expected_row:
                continue
            parsed = json.loads(line)
            assert parsed.keys() == {"@timestamp", "clientip", "request", "status", "size"}
            assert line.endswith(b"\n") and line.isascii() and parsed["request"].isascii()
            ts = parsed["@timestamp"] * 1000
            assert not docs or docs[-1][0] <= ts
            status = parsed["status"]
            size = parsed["size"]
            request = parsed["request"].encode("ascii")
            src = line[:-1]
            ip = ipaddress.IPv4Address(parsed["clientip"]).packed
            wire.write(HEADER.pack(row, ts, status, size, ip, len(request), len(src)))
            wire.write(request)
            wire.write(src)
            out.write(str(row).encode("ascii") + b"\t" + line)
            sampled_sources.update(src + b"\n")
            key = (ts, pos)
            docs.append(key)
            statuses[status] += 1
            seen_ts[ts] += 1
            size_min, size_max = min(size_min, size), max(size_max, size)
            omitted += len(request) > 256
            if parsed["request"] == TERM:
                by_case["term"].append(key)
            if START <= ts < END:
                by_case["range"].append(key)
                if status in (200, 400):
                    by_case[f"range{status}"].append(key)
            pos += 1
            expected_row = 1 + pos * TOTAL // N

    assert row == TOTAL and pos == N
    assert decompressed.hexdigest() == provenance["archive"]["uncompressed_sha256"]
    assert all(by_case.values())
    assert docs[-1][0] < OFFICIAL_CURSOR
    mid = docs[N // 2][0]
    cursor = min((ts for ts, n in seen_ts.items() if n >= 10),
                 key=lambda ts: (abs(ts - mid), ts))
    assert docs[0][0] < cursor < docs[-1][0]
    oracle = {
        "counts": {"all": N, **{k: len(v) for k, v in by_case.items()}},
        "hits": {"all": [doc for _, doc in docs[:10]],
                 **{k: [doc for _, doc in by_case[k][:10]] for k in ("term", "range200", "range400")}},
        "stable_pages": {},
    }
    for rev, direction in ((False, "asc"), (True, "desc")):
        ordered_docs = ordered(docs, rev)
        oracle["hits"][direction] = ordered_docs[:10]
        for name, cut in (("official", OFFICIAL_CURSOR), ("sample", cursor)):
            after = [pair for pair in ordered_docs if (pair[0] < cut if rev else pair[0] > cut)]
            oracle["hits"][f"{name}_{direction}"] = after[:10]
        oracle["stable_pages"][direction] = [ordered_docs[i:i + 10] for i in range(0, 40, 10)]
    save(data / "oracle.json", oracle)
    save(data / "input.json", {
        "track_commit": provenance["track_commit"],
        "source": provenance["source"],
        "source_compressed_sha256": compressed_sha,
        "source_uncompressed_sha256": decompressed.hexdigest(),
        "selection": "1 + floor(i * 2708746 / 1000000), i=0..999999, ascending original row",
        "records": N,
        "first_last_row": [1, 1 + (N - 1) * TOTAL // N],
        "tsv_sha256": rt.sha(rows),
        "binary_sha256": rt.sha(binary),
        "stored_source_lines_sha256": sampled_sources.hexdigest(),
        "wire": "big-endian u32 row, i64 timestamp_ms, i32 status, u32 size, IPv4 bytes, u16 request bytes, u16 source bytes, request UTF-8, original JSON line UTF-8 without LF",
        "timestamps_ms": [docs[0][0], docs[-1][0]],
        "timestamps_utc": [datetime.fromtimestamp(docs[i][0] / 1000, timezone.utc).isoformat() for i in (0, -1)],
        "distinct_timestamps": len(seen_ts),
        "seconds_with_ties": sum(n > 1 for n in seen_ts.values()),
        "status_counts": dict(sorted(statuses.items())),
        "size_min_max": [size_min, size_max],
        "ignored_raw_docs": omitted,
        "range_start_ms_inclusive": START,
        "range_end_ms_exclusive": END,
        "official_cursor_ms": OFFICIAL_CURSOR,
        "sample_cursor_ms": cursor,
        "sample_cursor_equal_docs": seen_ts[cursor],
        "counts": oracle["counts"],
    })

def prepare(args):
    manifest = rt.load(MANIFEST)
    spec = manifest["archive"]
    source = args.archive or args.destination / spec["file"]
    if not args.archive:
        rt.download(manifest["source"], source, spec["sha256"])
    rt.require(source.stat().st_size == spec["bytes"] and rt.sha(source) == spec["sha256"],
               "Wrong Rally shard: " + str(source))
    existing = {name for name in manifest["files"] if (args.destination / name).exists()}
    if existing:
        rt.require(existing == set(manifest["files"]), "Incomplete prepared Rally dataset; use a fresh destination")
        verify(args.destination, manifest, include_tsv=True)
        print("READY", args.destination, flush=True)
        return
    # Keep a partial transform for investigation if data or hashing fails.
    staged = Path(tempfile.mkdtemp(prefix="prepare-", dir=args.destination))
    transform(source, staged, manifest)
    verify(staged, manifest, include_tsv=True)
    for name in manifest["files"]:
        (staged / name).rename(args.destination / name)
    staged.rmdir()
    print("READY", args.destination, flush=True)


def verify(directory, manifest, include_tsv=False):
    for name, expected in manifest["files"].items():
        if name == "million.tsv" and not include_tsv:
            continue
        path = directory / name
        rt.require(path.is_file() and path.stat().st_size == expected["bytes"]
                   and rt.sha(path) == expected["sha256"], "Rally input identity mismatch: " + str(path))
