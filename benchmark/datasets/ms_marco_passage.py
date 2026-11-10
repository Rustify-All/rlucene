#!/usr/bin/env python3
"""Prepare source-ordered MS MARCO Passage Ranking v1 inputs."""
import hashlib
import json
from pathlib import Path
import struct
import tarfile
import tempfile

from common import runtime as rt

ROWS = 8_841_823
SMALL = 4096
FULL = 1_000_000
MANIFEST = Path(__file__).resolve().parent / "ms-marco-passage/manifest.json"
FIXTURES = [
    "zqfixture zqboundary zqfixture café Cafe\u0301",
    "zqfixture\tzqboundary 你好 世界 👩🏽‍🔬",
    "zqboundary zqfixture Straße İSTANBUL KELVIN",
    "zqfixture zqbetween zqboundary can't O’Neill 3.14",
    "𐐀𐐨 𠀀 emoji 👨‍👩‍👧‍👦 🇨🇳",
    "",
    " \t  \u00a0",
    "zero\u0000byte CRLF\r\nembedded \u2028line separator",
    "left " + "x" * 254 + " right",
    "left " + "x" * 255 + " right",
    "left " + "x" * 256 + " right",
    "left " + "𐐀" * 128 + " right",
    "scoretie sample",
    "scoretie sample",
]


def write_json(path, data):
    path.write_text(json.dumps(data, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")


def record(pid, text):
    return struct.pack(">II", len(pid), len(text)) + pid + text


def targeted(first, fixture=False):
    cases = [
        ["pid-first", "pid", first], ["pid-missing", "pid", "999999999"],
        ["term-the", "term", "the"], ["term-missing", "term", "zqneverpresent"],
        ["phrase-of-the", "phrase", ["of", "the"]],
    ]
    if fixture:
        cases += [
            ["pid-unicode", "pid", str(ROWS + 4)],
            ["pid-empty", "pid", str(ROWS + 5)],
            ["fixture-term", "term", "zqfixture"],
            ["fixture-phrase", "phrase", ["zqfixture", "zqboundary"]],
            ["fixture-cjk", "term", "你"],
            ["fixture-tie", "term", "scoretie"],
        ]
    return cases


def fixtures(data):
    data.mkdir(parents=True, exist_ok=True)
    with (data / "fixture.bin").open("wb") as output:
        output.write(struct.pack(">I", len(FIXTURES)))
        for i, value in enumerate(FIXTURES):
            output.write(record(str(ROWS + i).encode(), value.encode()))
    write_json(data / "fixture-queries.json", targeted(str(ROWS), True))
    write_json(data / "fixture-tokens.json", FIXTURES)


def verify(data, manifest, include_dev=False):
    for name, expected in manifest["files"].items():
        if name == manifest["dev_queries"]["member"] and not include_dev:
            continue
        path = data / name
        rt.require(path.is_file() and path.stat().st_size == expected["bytes"] and rt.sha(path) == expected["sha256"],
                   f"Prepared MS MARCO file mismatch: {path}")


def transform(collection, queries_path, stage, manifest):
    fixtures(stage)
    source = manifest["collection_tsv"]
    seen = bytearray(ROWS)
    source_sha = hashlib.sha256()
    source_bytes = non_ascii = extra_tabs = max_bytes = row_count = 0
    sequential = True
    samples = {n: 0 for n in (SMALL, FULL)}
    first = None
    token_cases = []
    with tarfile.open(collection, "r|gz") as archive, \
            (stage / "million.bin").open("wb") as million, \
            (stage / "small.bin").open("wb") as small:
        member = archive.next()
        if member is None or member.name != source["member"] or not member.isfile():
            raise ValueError(f"Unexpected collection archive member: {member}")
        outputs = {SMALL: small, FULL: million}
        for n, stream in outputs.items():
            stream.write(struct.pack(">I", n))
        for row, raw in enumerate(archive.extractfile(member)):
            row_count += 1
            source_sha.update(raw)
            source_bytes += len(raw)
            if not raw.endswith(b"\n"):
                raise ValueError(f"Missing LF at collection row {row + 1}")
            pid, sep, passage = raw[:-1].partition(b"\t")
            if not sep or not pid.isdigit():
                raise ValueError(f"Invalid pid or TSV separator at row {row + 1}")
            number = int(pid)
            if number >= ROWS or seen[number] or pid != str(number).encode("ascii"):
                raise ValueError(f"Invalid or duplicate pid at row {row + 1}")
            seen[number] = 1
            sequential &= number == row
            passage.decode("utf-8", errors="strict")
            non_ascii += not passage.isascii()
            extra_tabs += b"\t" in passage
            max_bytes = max(max_bytes, len(passage))
            if row == 0:
                first = pid.decode("ascii")
            for n, stream in outputs.items():
                i = samples[n]
                if i < n and row == i * ROWS // n:
                    stream.write(record(pid, passage))
                    samples[n] += 1
                    if n == SMALL and len(token_cases) < 48 and (not passage.isascii() or len(token_cases) < 12):
                        token_cases.append(passage.decode("utf-8"))
            if row_count % 1_000_000 == 0:
                print(f"Prepared {row_count:,} source rows", flush=True)
        if archive.next() is not None:
            raise ValueError("Unexpected extra member in collection archive")
    if row_count != ROWS or samples != {SMALL: SMALL, FULL: FULL}:
        raise ValueError(f"Collection row or sample count mismatch: {row_count}, {samples}")

    observed = {"member": member.name, "rows": row_count, "bytes": source_bytes,
                "sha256": source_sha.hexdigest(), "non_ascii_passages": non_ascii,
                "passages_with_additional_tabs": extra_tabs, "max_passage_utf8_bytes": max_bytes,
                "pid_equals_zero_based_source_row": sequential}
    rt.require(observed == source, "Uncompressed MS MARCO collection identity mismatch")

    dev_spec = manifest["dev_queries"]
    with tarfile.open(queries_path, "r:gz") as queries:
        dev = queries.extractfile(dev_spec["member"]).read()
    (stage / dev_spec["member"]).write_bytes(dev)
    rows = [line.split("\t", 1) for line in dev.decode("utf-8").splitlines()]
    if len(rows) != dev_spec["rows"] or len({qid for qid, _ in rows}) != len(rows):
        raise ValueError(f"Unexpected official dev layout: {len(rows)} queries")
    count = dev_spec["selected"]
    dev_cases = [[f"dev-{qid}", "dev", query] for qid, query in (rows[i * len(rows) // count] for i in range(count))]
    write_json(stage / "small-queries.json", targeted(first) + dev_cases)
    write_json(stage / "million-queries.json", targeted(first) + dev_cases)
    write_json(stage / "small-tokens.json", token_cases)


def archive(data, name, provided, manifest):
    spec = manifest["archives"][name]
    if provided is None:
        provided = data / name
        rt.download(spec["source"], provided, spec["sha256"])
    rt.require(provided.is_file() and provided.stat().st_size == spec["bytes"] and rt.sha(provided) == spec["sha256"],
               f"Wrong MS MARCO archive: {provided}")
    return provided


def prepare(args):
    manifest = rt.load(MANIFEST)
    data = args.destination
    existing = {name for name in manifest["files"] if (data / name).exists()}
    if not existing:
        collection = archive(data, "collection.tar.gz", args.archive, manifest)
        queries = archive(data, "queries.tar.gz", args.queries_archive, manifest)
        stage = Path(tempfile.mkdtemp(prefix="derive-", dir=data))
        print("PREPARE", stage, flush=True)
        transform(collection, queries, stage, manifest)
        verify(stage, manifest, include_dev=True)
        for name in manifest["files"]:
            (stage / name).rename(data / name)
        stage.rmdir()
    verify(data, manifest, include_dev=True)
    print("READY", data, flush=True)
