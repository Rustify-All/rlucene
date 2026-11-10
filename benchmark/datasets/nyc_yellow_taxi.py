#!/usr/bin/env python3
"""Derive fixed, ordered records and independent query oracles from the TLC Parquet."""
import json
from pathlib import Path
import struct
import sys

import numpy as np
import pyarrow as pa
import pyarrow.compute as pc
import pyarrow.parquet as pq

from common import runtime as rt

FIELDS = ("tpep_pickup_datetime", "tpep_dropoff_datetime", "PULocationID", "DOLocationID",
          "trip_distance", "total_amount")
DTYPES = (">i8", ">i8", ">i4", ">i4", ">i8", ">i8")
RECORD = np.dtype([("id", ">i8"), ("mask", "u1"), *zip(FIELDS, DTYPES)])
FLUSH = 65536
MILLION = 1_000_000
START = int(np.datetime64("2025-01-01", "us").astype(np.int64))
MID = int(np.datetime64("2025-01-15", "us").astype(np.int64))
END = int(np.datetime64("2025-02-01", "us").astype(np.int64))
DAY = 86_400_000_000
MAX_LONG = (1 << 63) - 1
CANONICAL_NAN = 0x7FF8000000000000
MISSING = (MAX_LONG, MAX_LONG, (1 << 31) - 1, (1 << 31) - 1, 0, 0)


def save(path, value):
    path.write_text(json.dumps(value, ensure_ascii=False, indent=2, allow_nan=False) + "\n", encoding="utf-8")


def bits(value):
    return struct.unpack(">q", struct.pack(">d", value))[0]


def sortable_double(values):
    values = np.asarray(values, dtype=np.int64)
    canonical = np.where(np.isnan(values.view(np.float64)), CANONICAL_NAN, values)
    return canonical ^ ((canonical >> 63) & MAX_LONG)


def double_range(field, lower, upper):
    return (field, bits(lower), bits(upper))


def cases(ids):
    return [
        ("all", [], []),
        ("id-first", [(6, int(ids[0]), int(ids[0]))], []),
        ("id-middle", [(6, int(ids[len(ids)//2]), int(ids[len(ids)//2]))], []),
        ("id-last", [(6, int(ids[-1]), int(ids[-1]))], []),
        ("id-absent", [(6, -1, -1)], []),
        ("pickup-first-day", [(0, START, START + DAY - 1)], []),
        ("dropoff-midday", [(1, MID, MID + DAY // 2 - 1)], []),
        ("both-in-january", [(0, START, END - 1), (1, START, END - 1)], []),
        ("pickup-before-january", [(0, -(1 << 63), START - 1)], []),
        ("dropoff-after-january", [(1, END, MAX_LONG)], []),
        ("pickup-february-boundary", [(0, END, END)], []),
        ("zones-161-236", [(2, 161, 161), (3, 236, 236)], []),
        ("zones-132-138", [(2, 132, 132), (3, 138, 138)], []),
        ("pickup-zone-0", [(2, 0, 0)], []),
        ("zone-and-time", [(2, 161, 161), (0, START, END - 1)], [(0, False)]),
        ("miles-1-to-5", [double_range(4, 1.0, 5.0)], []),
        ("miles-negative-zero", [double_range(4, -0.0, -0.0)], []),
        ("miles-positive-zero", [double_range(4, +0.0, +0.0)], []),
        ("miles-nan", [(4, CANONICAL_NAN, CANONICAL_NAN)], []),
        ("miles-infinite", [double_range(4, float("inf"), float("inf"))], []),
        ("amount-refunds", [double_range(5, -100.0, -0.0)], []),
        ("amount-20-to-40", [double_range(5, 20.0, 40.0)], []),
        ("fare-distance-zones", [double_range(4, 1.0, 5.0), double_range(5, 20.0, 40.0),
                                 (2, 100, 200), (3, 200, 250)], [(5, True), (4, False)]),
        ("pickup-asc", [], [(0, False)]),
        ("dropoff-desc", [], [(1, True)]),
        ("zone-asc-zone-desc", [], [(2, False), (3, True)]),
        ("distance-asc", [], [(4, False)]),
        ("distance-desc", [], [(4, True)]),
        ("amount-asc", [], [(5, False)]),
        ("amount-desc", [], [(5, True)]),
        *[(f"missing-{name}", [(i, None, None)], [(5, False)])
          for i, name in enumerate(FIELDS)],
        ("missing-distance-and-amount", [(4, None, None), (5, None, None)], []),
    ]


def oracle(records, queries):
    n = len(records)
    ids = records["id"]
    present = [(records["mask"] & (1 << field)) != 0 for field in range(6)]
    actual = [records[name].astype(np.int64) for name in FIELDS]
    actual[4:6] = [sortable_double(v) for v in actual[4:6]]
    actual.append(ids)
    sort_values = [np.where(mask, value, missing) for mask, value, missing in zip(present, actual, MISSING)]
    results = []
    for name, clauses, sorts in queries:
        include = np.ones(n, dtype=bool)
        for field, lower, upper in clauses:
            if lower is None:
                include &= ~present[field]
            else:
                if field in (4, 5):
                    lower, upper = sortable_double([lower, upper])
                include &= (actual[field] >= lower) & (actual[field] <= upper)
                if field != 6:
                    include &= present[field]
        docs = np.flatnonzero(include)
        if sorts:
            keys = [docs]
            for field, reverse in reversed(sorts):
                key = sort_values[field][docs]
                keys.append(~key if reverse else key)
            first = docs[np.lexsort(keys)[:10]]
        else:
            first = docs[:10]
        hits = [[int(doc), int(ids[doc])] for doc in first]
        results.append([name, len(docs), hits])
    full, tail = divmod(n, FLUSH)
    return {"docs": n, "live": n, "segments": [FLUSH] * full + ([tail] if tail else []),
            "results": results}


def write_input(destination, stem, records):
    assert RECORD.itemsize == 49 and np.all(records["mask"] <= 63)
    assert np.all(records["id"][1:] > records["id"][:-1])
    with (destination / f"{stem}.bin").open("wb") as stream:
        stream.write(struct.pack(">I", len(records)))
        records.tofile(stream)
    queries = cases(records["id"])
    expected = oracle(records, queries)
    save(destination / f"{stem}-queries.json", queries)
    save(destination / f"{stem}-expected.json", expected)
    return expected


def write_fixture(destination):
    regular = (START, START + 1, 161, 236, 1.0, 20.0)
    payloads = [
        regular, regular,
        (START - 1, START, 0, 265, -0.0, -0.0),
        (START + DAY - 1, END, 132, 138, +0.0, +0.0),
        (START + DAY, END + 1, 161, 236, 5.0, 40.0),
        (MID, MID, 132, 138, -2.5, -100.0),
        (END, END - 1, 264, 265, float("inf"), float("-inf")),
        (END + 1, END + 2, 265, 264, float("-inf"), float("inf")),
        (None, None, None, None, None, None),
        (START, None, 161, None, None, 40.0),
        (None, MID + DAY // 2 - 1, None, 236, 5.0, None),
        (MID + DAY // 2, MID + DAY // 2, 200, 250, 5.0, 40.0),
        (START + 3, START + 2, -1, 266, 5.000000000000001, -100.00000000000001),
        (0, -1, 161, 236, float("nan"), float("nan")),
        (START, START, 161, 236, struct.unpack(">d", bytes.fromhex("fff8000000000011"))[0], None),
        (START, START, -(1 << 31), (1 << 31) - 1, 0.9999999999999999, 19.999999999999996),
    ]
    records = np.zeros(len(payloads), dtype=RECORD)
    for i, values in enumerate(payloads):
        records["id"][i] = 100 + 3 * i
        for f, value in enumerate(values):
            if value is not None:
                records["mask"][i] |= 1 << f
                records[FIELDS[f]][i] = bits(value) if f >= 4 else value
    expected = {row[0]: row[1:] for row in write_input(destination, "fixture", records)["results"]}
    assert expected["miles-negative-zero"] == [1, [[2, 106]]]
    assert expected["miles-positive-zero"] == [1, [[3, 109]]]
    assert expected["miles-nan"] == [2, [[13, 139], [14, 142]]]
    assert expected["missing-distance-and-amount"] == [1, [[8, 124]]]
    assert expected["pickup-asc"][1][:5] == [[13, 139], [2, 106], [0, 100], [1, 103], [9, 127]]


def payload(table, ids):
    records = np.zeros(len(table), dtype=RECORD)
    records["id"] = ids
    for i, name in enumerate(FIELDS):
        column = table[name].combine_chunks()
        present = column.is_valid().to_numpy(zero_copy_only=False)
        records["mask"] |= present.astype(np.uint8) << i
        if i < 2:
            column = column.cast(pa.int64())
        elif i >= 4:
            column = column.view(pa.int64())
        records[name] = pc.fill_null(column, 0).to_numpy(zero_copy_only=False)
    return records


def distributions(table):
    result = {}
    for field in table.schema:
        col = table[field.name].combine_chunks()
        valid = col.is_valid().to_numpy(zero_copy_only=False)
        stats = {"null": col.null_count}
        if pa.types.is_timestamp(field.type):
            col = col.cast(pa.int64())
        if pa.types.is_integer(col.type) or pa.types.is_floating(col.type):
            values = pc.fill_null(col, 0).to_numpy(zero_copy_only=False)[valid]
            finite = np.isfinite(values)
            stats.update(zero=int(np.count_nonzero(values == 0)), negative=int(np.count_nonzero(values < 0)),
                         nan=int(np.count_nonzero(np.isnan(values))),
                         positive_infinity=int(np.count_nonzero(np.isposinf(values))),
                         negative_infinity=int(np.count_nonzero(np.isneginf(values))))
            if finite.any():
                stats.update(min_finite=values[finite].min().item(), max_finite=values[finite].max().item())
            if pa.types.is_timestamp(field.type):
                stats.update(before_month=int(np.count_nonzero(values < START)),
                             after_month=int(np.count_nonzero(values >= END)))
            if pa.types.is_floating(field.type):
                stats["negative_zero"] = int(np.count_nonzero((values == 0) & np.signbit(values)))
            if field.name in FIELDS[2:4]:
                stats["outside_1_to_265"] = int(np.count_nonzero((values < 1) | (values > 265)))
        result[field.name] = stats
    pickup = table[FIELDS[0]].combine_chunks().cast(pa.int64())
    dropoff = table[FIELDS[1]].combine_chunks().cast(pa.int64())
    backwards = pc.sum(pc.less(dropoff, pickup)).as_py() or 0
    return {"rows": len(table), "by_column": result, "dropoff_before_pickup": backwards}


def duplicate_counts(table):
    # Compare all float payloads by bits and timestamps by stored integers.
    arrays = []
    for name in table.column_names:
        column = table[name]
        if pa.types.is_floating(column.type) or pa.types.is_timestamp(column.type):
            column = pa.chunked_array([chunk.view(pa.int64()) for chunk in column.chunks], type=pa.int64())
        arrays.append(column)
    exact = pa.Table.from_arrays(arrays, names=table.column_names)
    counts = exact.group_by(exact.column_names, use_threads=False).aggregate([([], "count_all")])["count_all"]
    repeated = pc.filter(counts, pc.greater(counts, 1))
    return {"distinct": len(counts), "duplicate_excess": len(table) - len(counts),
            "rows_in_duplicate_groups": pc.sum(repeated).as_py() or 0}


def derive(archive, staging, metadata):
    parquet = pq.ParquetFile(archive)
    expected_types = (pa.timestamp("us"), pa.timestamp("us"), pa.int32(), pa.int32(), pa.float64(), pa.float64())
    for name, expected in zip(FIELDS, expected_types):
        if parquet.schema_arrow.field(name).type != expected:
            raise ValueError(f"Unexpected schema: {name}")
    for name in FIELDS[:2]:
        logical = json.loads(parquet.schema.column(parquet.schema.names.index(name)).logical_type.to_json())
        if logical["isAdjustedToUTC"] or logical["timeUnit"] != "microseconds":
            raise ValueError(f"Unexpected timestamp semantics: {logical}")

    table = parquet.read(use_threads=False)
    n = len(table)
    assert n == parquet.metadata.num_rows == 3_475_226
    ids = np.arange(MILLION, dtype=np.int64) * n // MILLION
    sample = table.take(pa.array(ids))
    records = payload(sample, ids)
    small_ids = np.arange(4096, dtype=np.int64) * MILLION // 4096
    write_input(staging, "million", records)
    write_input(staging, "small", records[small_ids])

    distribution = {"source": distributions(table), "sample": distributions(sample)}
    for name, frame in (("source", table), ("sample", sample)):
        distribution[name]["duplicates_all_columns"] = duplicate_counts(frame)
        distribution[name]["duplicates_indexed_payload"] = duplicate_counts(frame.select(FIELDS))
    masks, counts = np.unique(records["mask"], return_counts=True)
    distribution["sample"]["presence_mask_histogram"] = {str(int(k)): int(v) for k, v in zip(masks, counts)}
    save(staging / "distribution.json", distribution)

    meta = parquet.metadata
    snapshot = {
        **metadata["archive"],
        "rows": n, "columns": meta.num_columns, "created_by": meta.created_by,
        "format_version": meta.format_version,
        "row_groups": [meta.row_group(i).num_rows for i in range(meta.num_row_groups)],
        "arrow_schema": [{"name": f.name, "type": str(f.type), "nullable": f.nullable} for f in table.schema],
        "parquet_columns": [
            {"name": col.name, "physical": col.physical_type, "logical": json.loads(col.logical_type.to_json())}
            for col in (parquet.schema.column(i) for i in range(meta.num_columns))
        ],
    }
    return {
        "snapshot": snapshot, "documents": MILLION, "generator_sha256": rt.sha(Path(__file__)),
        "python": sys.version, "pyarrow": pa.__version__, "numpy": np.__version__,
        "sample_rule": "zero-based floor(i * source_rows / 1000000); original Parquet row order",
        "small_rule": "zero-based floor(i * 1000000 / 4096) into the million",
        "input": {"header": "big-endian u32 row count", "record_bytes": RECORD.itemsize,
                  "layout": RECORD.descr, "mask_bit_order": FIELDS,
                  "absent_values": "zero payload plus cleared bit; omit both fields",
                  "timestamps": "physical signed microseconds on the local calendar epoch; no timezone conversion",
                  "doubles": "signed i64 containers of original IEEE 754 bits"},
    }
