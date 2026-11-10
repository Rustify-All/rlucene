#!/usr/bin/env python3
"""Prepare deterministic GeoNames rows and independent per-query expectations."""
from collections import Counter
from hashlib import sha256
import heapq
from itertools import islice
import json
import math
from pathlib import Path
import struct
from zipfile import ZipFile

from common import runtime as rt

MILLION = 1_000_000
SMALL = 4096
FLUSH = 65_536
COLUMNS = ("geonameid name asciiname alternatenames latitude longitude feature_class feature_code "
           "country_code cc2 admin1 admin2 admin3 admin4 population elevation dem timezone "
           "modification_date").split()
LAT = 1.0 / (2**32 / 180.0)
LON = 1.0 / (2**32 / 360.0)
FIXTURE = [
    ("1", -90, -180, 0, ["南極", "same", "same"]),
    ("2", 90, 180, -1, []),
    ("3", 0, 0, 0, ["zero", "same"]),
    ("4", -0.0, -0.0, 1, []),
    ("5", 0.000000001, 0.000000001, 100, ["東京", "München", "𐐀"]),
    ("6", -0.000000001, -0.000000001, 100, ["東京", "München"]),
    ("7", 42, 179.9, 100, ["east", "same"]),
    ("8", 42, -179.9, 100, ["west", "same"]),
    ("9", 42, 169.9, 5_000_000_000, ["outer"]),
    ("10", 48.85, 2.35, 100, ["Paris", "Paris"]),
    ("11", 43, 3, -1, ["missing-pop"]),
    ("12", 0, 179.99999999, 10, ["north"]),
    ("13", -15, -180, 20, ["south"]),
]


def save(path, value):
    path.write_text(json.dumps(value, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")


def scale(value, step):
    upper = 90 if step == LAT else 180
    if value == upper:
        value = math.nextafter(value, -math.inf)
    return value / step


def encode(lat, lon):
    return math.floor(scale(lat, LAT)), math.floor(scale(lon, LON))


def coord(value, limit):
    result = float(value)
    if not -limit <= result <= limit:
        raise ValueError(f"Invalid WGS84 coordinate: {value!r}")
    return result


def parse(raw):
    parts = raw.decode("utf-8", errors="strict").rstrip("\r\n").split("\t")
    if len(parts) != len(COLUMNS):
        raise ValueError(f"Expected 19 fields for {parts[0]!r}, got {len(parts)}")
    ident, _, _, alt, lat, lon = parts[:6]
    population = parts[14]
    if not ident.isascii() or not ident.isdigit() or str(int(ident)) != ident or ident == "0":
        raise ValueError(f"Invalid ID: {ident!r}")
    if population and (not population.isascii() or not population.isdigit()):
        raise ValueError(f"Invalid population: {population!r}")
    pop = int(population) if population else -1
    aliases = [a for a in alt.split(",") if a]
    return (ident, coord(lat, 90), coord(lon, 180), pop, aliases), parts


def write_row(out, row, encoded):
    ident, lat, lon, pop, aliases = row
    raw_id = ident.encode("ascii")
    out.write(struct.pack(">ddiiqH", lat, lon, *encoded, pop, len(raw_id)))
    out.write(raw_id)
    out.write(struct.pack(">H", len(aliases)))
    for alias in aliases:
        raw = alias.encode("utf-8")
        if len(raw) > 32_766:
            raise ValueError(f"Alias length exceeds Lucene limit: {ident}")
        out.write(struct.pack(">H", len(raw)))
        out.write(raw)


def haversine(a, b, x, y):
    lat1, lon1, lat2, lon2 = map(math.radians, (a, b, x, y))
    h = math.sin((lat1 - lat2) / 2)**2 + math.cos(lat1) * math.cos(lat2) * math.sin((lon1 - lon2) / 2)**2
    return 2 * 6_371_008.7714 * math.asin(math.sqrt(min(1, max(0, h))))


class Oracle:
    def __init__(self, fixture):
        self.cases = [
            ["world", "box", [-90, 90, -180, 180], False],
            ["antimeridian", "box", [-90, 90, 170, -170], False],
            ["antimeridian-edge", "box", [-90, 90, 180, -179.5], False],
            ["north-pole-empty", "box", [90, 90, -180, 180], False],
            ["southwest-edge", "box", [-90, -90, -180, -180], False],
            ["zero-cell", "box", [0, 0, 0, 0], False],
            ["paris-area", "box", [48, 50, 1, 4], True],
            ["near-paris", "distance", [48.8566, 2.3522, 125_000], False],
            ["near-dateline", "distance", [42, 180, 250_000], False],
            ["zero-pop", "pop", [0, 0], False],
            ["population-filter-sort", "pop", [1, 2**63 - 1], True],
            ["population-sort", "all", None, True],
            ["population-ties", "pop", [100, 100], True],
            ["alias-tokyo", "alias", "東京", False],
            ["alias-latin", "alias", "München", False],
            ["alias-absent", "alias", "__geonames_fixture_absent__", False],
            ["id-absent", "id", "0", False],
        ]
        if fixture:
            self.cases.extend([
                ["fixture-duplicate", "alias", "same", False],
                ["fixture-supplementary", "alias", "𐐀", False],
                ["fixture-missing-pop", "id", "2", False],
                ["fixture-missing-last", "box", [42, 49, 2, 4], True],
            ])
        self.tests = [self.compile(kind, value) for _, kind, value, _ in self.cases]
        self.counts = [0 for _ in self.cases]
        self.best = [[] for _ in self.cases]
        self.margin = {}

    @staticmethod
    def compile(kind, value):
        if kind == "box":
            a, b, c, d = value
            if a == 90 or c == d == 180:
                return lambda *_: False
            if c == 180:
                c = -180
            low_lat, high_lat = math.ceil(scale(a, LAT)), math.floor(scale(b, LAT))
            low_lon, high_lon = math.ceil(scale(c, LON)), math.floor(scale(d, LON))
            if c <= d:
                return lambda row, enc: low_lat <= enc[0] <= high_lat and low_lon <= enc[1] <= high_lon
            return lambda row, enc: low_lat <= enc[0] <= high_lat and (enc[1] >= low_lon or enc[1] <= high_lon)
        if kind == "pop":
            return lambda row, enc: row[3] >= 0 and value[0] <= row[3] <= value[1]
        if kind == "alias":
            return lambda row, enc: value in row[4]
        if kind == "id":
            return lambda row, enc: value == row[0]
        if kind == "all":
            return lambda *_: True
        return None

    def add(self, doc, row, encoded):
        for i, (name, kind, value, ordered) in enumerate(self.cases):
            if kind == "distance":
                a, b, radius = value
                distance = haversine(a, b, encoded[0] * LAT, encoded[1] * LON)
                self.margin[name] = min(self.margin.get(name, math.inf), abs(distance - radius))
                matches = distance <= radius
            else:
                matches = self.tests[i](row, encoded)
            if not matches:
                continue
            self.counts[i] += 1
            best = self.best[i]
            entry = (row[3] if ordered else 0, -doc, row[0])
            if len(best) < 10:
                heapq.heappush(best, entry)
            elif entry > best[0]:
                heapq.heapreplace(best, entry)

    def write(self, out, stem, n, first, last):
        if any(m < 0.01 for m in self.margin.values()):
            raise ValueError(f"Distance oracle found a point within 1 cm of radius: {self.margin}")
        rows = [[name, count, [[-doc, ident] for _, doc, ident in sorted(best, reverse=True)]]
                for (name, *_), count, best in zip(self.cases, self.counts, self.best)]
        for name, ident, doc in [("id-first", first, 0), ("id-last", last, n - 1)]:
            self.cases.append([name, "id", ident, False])
            rows.append([name, 1, [[doc, ident]]])
        segments = [min(FLUSH, n - offset) for offset in range(0, n, FLUSH)]
        save(out / f"{stem}-queries.json", self.cases)
        save(out / f"{stem}-expected.json", {"docs": n, "live": n, "segments": segments, "results": rows})
        return self.margin


class Stats:
    def __init__(self):
        self.counts = Counter()
        self.missing = Counter()
        self.alias_count = Counter()
        self.feature_class = Counter()
        self.country = Counter()
        self.names = Counter()
        self.ids = set()
        self.max_alias = (0, None)
        self.max_alias_bytes = (0, None)
        self.bounds = [90, -90, 180, -180]
        self.max_population = 0

    def add(self, row, columns):
        ident, lat, lon, pop, aliases = row
        if ident in self.ids:
            raise ValueError(f"Duplicate geonameid {ident}")
        self.ids.add(ident)
        self.names[columns[1]] += 1
        self.missing.update(name for name, v in zip(COLUMNS, columns) if not v)
        self.feature_class[columns[6]] += 1
        self.country[columns[8]] += 1
        self.alias_count[len(aliases)] += 1
        c = self.counts
        c["rows"] += 1
        c["zero_population"] += pop == 0
        c["positive_population"] += pop > 0
        c["zero_latitude"] += lat == 0
        c["zero_longitude"] += lon == 0
        c["both_coordinates_zero"] += lat == lon == 0
        c["non_ascii_name"] += not columns[1].isascii()
        c["non_ascii_asciiname"] += not columns[2].isascii()
        c["rows_with_non_ascii_alias"] += any(not a.isascii() for a in aliases)
        c["alias_values"] += len(aliases)
        c["non_ascii_alias_values"] += sum(not a.isascii() for a in aliases)
        if columns[3]:
            c["empty_alias_items"] += columns[3].count(",") + 1 - len(aliases)
        duplicates = len(aliases) - len(set(aliases))
        c["duplicate_alias_items"] += duplicates
        c["rows_with_duplicate_aliases"] += duplicates > 0
        c["rows_with_alias_edge_whitespace"] += any(a != a.strip() for a in aliases)
        if len(aliases) > self.max_alias[0]:
            self.max_alias = len(aliases), ident
        if aliases:
            length = max(len(a.encode("utf-8")) for a in aliases)
            if length > self.max_alias_bytes[0]:
                self.max_alias_bytes = length, ident
        self.bounds = [min(lat, self.bounds[0]), max(lat, self.bounds[1]),
                       min(lon, self.bounds[2]), max(lon, self.bounds[3])]
        self.max_population = max(self.max_population, pop)

    def summary(self):
        return {
            "counts": self.counts,
            "missing_by_column": {c: self.missing[c] for c in COLUMNS},
            "alias_count_histogram": dict(sorted(self.alias_count.items())),
            "max_alias_count_and_id": self.max_alias,
            "max_alias_bytes_and_id": self.max_alias_bytes,
            "feature_class": self.feature_class, "country_code": self.country,
            "coordinate_bounds": self.bounds, "max_population": self.max_population,
            "distinct_primary_names": len(self.names),
            "repeated_primary_names": sum(n > 1 for n in self.names.values()),
            "rows_with_repeated_primary_names": sum(n for n in self.names.values() if n > 1),
            "most_common_primary_names": self.names.most_common(15),
        }


def raw_identity(archive):
    h = sha256()
    records, size = 0, 0
    with archive.open("allCountries.txt") as stream:
        for block in iter(lambda: stream.read(8 * 1024 * 1024), b""):
            h.update(block)
            records += block.count(b"\n")
            size += len(block)
            end = block[-1:]
    if end != b"\n":
        raise ValueError("Expected LF at EOF")
    return {"sha256": h.hexdigest(), "records": records, "bytes": size}


def write_fixture(out):
    fixture = Oracle(True)
    save(out / "fixture.json", FIXTURE)
    with (out / "fixture.bin").open("wb") as stream:
        stream.write(struct.pack(">I", len(FIXTURE)))
        for doc, row in enumerate(FIXTURE):
            encoded = encode(row[1], row[2])
            write_row(stream, row, encoded)
            fixture.add(doc, row, encoded)
    return fixture.write(out, "fixture", len(FIXTURE), FIXTURE[0][0], FIXTURE[-1][0])


def derive(archive_path, tmp, metadata):
    stats = Stats()
    with ZipFile(archive_path) as archive:
        raw = raw_identity(archive)
        total = raw["records"]
        rt.require(raw == metadata["raw_tsv"], "GeoNames TSV differs from the pinned snapshot")
        print("SOURCE", raw, flush=True)
        large, small = Oracle(False), Oracle(False)
        last = -1
        small_i = 0
        picked = sha256()
        with archive.open("allCountries.txt") as src, (tmp / "million.bin").open("wb") as big, \
                (tmp / "small.bin").open("wb") as little, (tmp / "small.tsv").open("wb") as tsv:
            big.write(struct.pack(">I", MILLION))
            little.write(struct.pack(">I", SMALL))
            for i in range(MILLION):
                line_no = i * total // MILLION
                line = next(islice(src, line_no - last - 1, None))
                last = line_no
                picked.update(line)
                row, columns = parse(line)
                encoded = encode(row[1], row[2])
                stats.add(row, columns)
                write_row(big, row, encoded)
                large.add(i, row, encoded)
                if small_i < SMALL and i == small_i * MILLION // SMALL:
                    tsv.write(line)
                    write_row(little, row, encoded)
                    small.add(small_i, row, encoded)
                    if small_i == 0:
                        first = row[0]
                    small_last = row[0]
                    small_i += 1
                if (i + 1) % 200_000 == 0:
                    print("PREPARED", i + 1, flush=True)
        assert small_i == SMALL
        margins = {
            "million": large.write(tmp, "million", MILLION, first, row[0]),
            "small": small.write(tmp, "small", SMALL, first, small_last),
        }
    margins["fixture"] = write_fixture(tmp)
    save(tmp / "distribution.json", stats.summary())
    rt.require(picked.hexdigest() == metadata["million_tsv_sha256"], "Selected GeoNames rows differ")
    return {"snapshot": metadata["archive"], "raw_tsv": raw, "million_tsv_sha256": picked.hexdigest(),
                "sampling": "zero-based floor(i*N/1000000), i=0..999999; small selects floor(j*1000000/4096) from million",
                "documents": {"million": MILLION, "small": SMALL, "fixture": len(FIXTURE)},
                "minimum_distance_to_radius_meters": margins,
                "prepare_sha256": rt.sha(Path(__file__))}
