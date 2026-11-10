"""Reproduce published GeoNames and NYC Taxi indexing runs on frozen RLucene."""
from collections import Counter
import hashlib
import importlib
from itertools import permutations
import os
from pathlib import Path
import shutil
import subprocess
import time

from common import runtime as rt
import indexing_inputs
import protocol

SUITE = Path(__file__).resolve().parent


def freeze_source(out, commit):
    archive = out / "source.tar"
    with archive.open("xb") as stream:
        subprocess.run(["git", "-C", rt.REPOSITORY, "archive", commit], stdout=stream, check=True)
    source = out / "source"
    source.mkdir()
    subprocess.run(["tar", "-xf", archive, "-C", source], check=True)
    return source


def source_identity(source, commit):
    entries = subprocess.check_output(["git", "-C", rt.REPOSITORY, "ls-tree", "-rz", commit]).split(b"\0")
    paths = set()
    for entry in filter(None, entries):
        meta, filename = entry.split(b"\t", 1)
        _, kind, digest = meta.split()
        path = source / os.fsdecode(filename)
        paths.add(path)
        if kind != b"blob":
            raise ValueError(f"Unexpected source entry {path}")
        content = os.fsencode(os.readlink(path)) if path.is_symlink() else path.read_bytes()
        actual = hashlib.sha1(f"blob {len(content)}\0".encode() + content).hexdigest().encode()
        if actual != digest:
            raise ValueError(f"Frozen file differs from Git: {path}")
    found = {p for p in source.rglob("*") if (p.is_file() or p.is_symlink()) and "__pycache__" not in p.parts}
    if paths != found:
        raise ValueError(f"Unexpected frozen files: {sorted(str(p) for p in found - paths)}")
    return {"commit": commit, "tracked_files": len(paths), "archive_sha256": rt.sha(source.parent / "source.tar")}


def checked(command, log, env=None, cwd=rt.REPOSITORY):
    protocol.append(log.parent / "commands.jsonl", {"log": log.name, "cwd": str(cwd), "argv": list(map(str, command))})
    protocol.checked(command, log, env, cwd)


def cargo_build(out, preset, name, rustflags=()):
    target = out / f"target-{name}"
    env = rt.environment()
    if rustflags:
        env.update(CARGO_PROFILE_RELEASE_LTO="thin", CARGO_PROFILE_RELEASE_CODEGEN_UNITS="16",
                   CARGO_ENCODED_RUSTFLAGS="\x1f".join(("-Ctarget-cpu=native", *rustflags)))
    protocol.append(out / "build-settings.jsonl",
                    {"build": name, "target": str(target), "flags": ["-Ctarget-cpu=native", *rustflags] if rustflags else [],
                     "lto": "thin" if rustflags else "default", "codegen_units": 16 if rustflags else "default"})
    print("BUILD", name, flush=True)
    checked(["cargo", "+1.98.1", "build", "--locked", "--release", "-v", "--manifest-path",
             out / "driver/rust/Cargo.toml", "--target-dir", target], out / (name + ".log"), env, out / "source")
    binary = out / name
    shutil.copy2(target / "release" / preset["rust_binary"], binary)
    return ["env", f"tests.seed={preset['seed']}", binary]


def build(out, args, preset):
    jdk = rt.jdk(args.java_home)
    pinned = rt.load(rt.BENCHMARK / "common/java-jars.json")
    jars_dir = rt.dependencies(args.jars_dir)
    jars = [jars_dir / item["file"] for item in pinned]
    jar_identity = {str(jar): rt.sha(jar) for jar in jars}
    for item, jar in zip(pinned, jars):
        rt.require(jar_identity[str(jar)] == item["sha256"], f"Pinned JAR mismatch: {jar}")
    classes = out / "classes"
    classes.mkdir()
    classpath = os.pathsep.join(map(str, jars))
    jdk_path = Path(jdk["home"])
    java = [jdk_path / "bin/java", "-Xms4g", "-Xmx4g", "-XX:+UseParallelGC",
            f"-Dtests.seed={preset['seed']}", "-cp", f"{classes}{os.pathsep}{classpath}", preset["java_class"]]
    checked([jdk_path / "bin/javac", "--release", "23", "-encoding", "UTF-8", "-cp", classpath,
             "-d", classes, out / "driver" / "java" / (preset["java_class"] + ".java")], out / "javac.log")
    return {"java": java, "rust": cargo_build(out, preset, "ordinary")}, jdk, jar_identity


def train(out, data, manifest, preset):
    profiles = out / "profiles"
    profiles.mkdir()
    compiler_profiles = out / "build-script-profiles"
    compiler_profiles.mkdir()
    instrumented = cargo_build(out, preset, "instrumented", (f"-Cprofile-generate={compiler_profiles}",))
    binary = data / "million.bin"
    command = [*instrumented[:2], f"LLVM_PROFILE_FILE={profiles}/%p-%m.profraw",
               instrumented[-1], "reuse", binary]
    started = time.monotonic()
    with protocol.server(command, out, "training") as (ready, ask):
        rt.require(ready["input_bytes"] == binary.stat().st_size, "PGO input size mismatch")
        for i in range(1, preset["pgo_training_writes"] + 1):
            sample = protocol.write(ask, out / f"training-index-{i}", preset)
            protocol.append(out / "training-samples.jsonl", {"write": i, "pid": ready["pid"], **sample})
            print("TRAIN", i, round(sample["total_ns"] / 1e9, 3), flush=True)
    elapsed = time.monotonic() - started
    raw = sorted(profiles.glob(f"{ready['pid']}-*.profraw"))
    rt.require(raw and len(raw) == len(list(profiles.iterdir())), "Unexpected PGO profile contributors")
    profdata = Path(rt.output(["rustc", "+1.98.1", "--print", "target-libdir"])).parent / "bin/llvm-profdata"
    rt.require(profdata.is_file(), "Install llvm-tools-preview for Rust 1.98.1")
    merged = profiles / "merged.profdata"
    checked([profdata, "merge", "-o", merged, *raw], out / "pgo-merge.log")
    checked([profdata, "show", merged], out / "pgo-profile.log")
    rt.save(out / "PGO.json", {"wall_seconds": elapsed, "writes": preset["pgo_training_writes"],
        "input_sha256": manifest["files"]["million.bin"]["sha256"], "workload_pid": ready["pid"],
        "profile_files": rt.files(profiles), "llvm_profdata_sha256": rt.sha(profdata)})
    for i in range(1, preset["pgo_training_writes"] + 1):
        shutil.rmtree(out / f"training-index-{i}")
    return cargo_build(out, preset, "optimized", (f"-Cprofile-use={merged}",))


def reopen(reader, index, cases, expected, out, label):
    query_log = out / f"query-{label}.json"
    checked([*reader, "query", index, cases], query_log)
    observed = rt.load(query_log)
    if expected != observed:
        rt.save(out / f"query-difference-{label}.json", {"expected": expected, "observed": observed})
        raise ValueError(f"{label}: query mismatch")
    protocol.append(out / "verification.jsonl", {"label": label, "docs": expected["docs"],
        "queries": len(observed["results"]), "ordered_hits": sum(len(row[2]) for row in observed["results"]),
        "sha256": rt.sha(query_log)})


def preflight(commands, data, stem, out):
    binary = data / f"{stem}.bin"
    cases = data / f"{stem}-queries.json"
    expected = rt.load(data / f"{stem}-expected.json")
    geometry = {"docs": expected["docs"], "segments": expected["segments"]}
    for engine, command in commands.items():
        index = out / f"preflight-{stem}-{engine}"
        with protocol.server([*command, "reuse", binary], out, f"write-{stem}-{engine}") as (ready, ask):
            rt.require(ready["input_bytes"] == binary.stat().st_size, "Preflight input size mismatch")
            protocol.write(ask, index, {"documents": expected["docs"]})
        log = out / f"checkindex-{stem}-{engine}.log"
        checked([*commands["java"][:-1], "org.apache.lucene.index.CheckIndex", index, "-level", "2"], log)
        rt.require("No problems were detected" in log.read_text(), f"CheckIndex failed: {index}")
        for name, reader in commands.items():
            label = f"{stem}-{engine}-reader-{name}"
            reopen(reader, index, cases, expected, out, label)
            audit = out / f"audit-{label}.json"
            checked([*reader, "audit", index, binary], audit)
            rt.require(rt.load(audit) == geometry, f"Full audit mismatch: {label}")
            protocol.append(out / "audit.jsonl", {"label": label, **geometry, "sha256": rt.sha(audit)})
        print("PREFLIGHT OK", stem, engine, expected["docs"], flush=True)
    return expected


def artifacts(out):
    names = ["ordinary", "instrumented", "optimized"]
    return {"binaries": {n: rt.sha(out / n) for n in names if (out / n).is_file()},
            "classes": rt.files(out / "classes"),
            "profiles": rt.files(out / "profiles") if (out / "profiles").is_dir() else {}}


def check(out, receipt):
    rows = protocol.read_rows(out / "samples.jsonl")
    engines = ("java", "rust", "tuned")
    orders = set(permutations(engines))
    assert len(rows) == 24
    wanted = set()
    for batch in range(1, 4):
        warm = [r for r in rows if r["batch"] == batch and r["phase"] == "warm"]
        formal = [r for r in rows if r["batch"] == batch and r["phase"] == "formal"]
        assert [r["round"] for r in warm] == [1, 2]
        assert [r["round"] for r in formal] == list(range(1, 7))
        assert {tuple(r["order"]) for r in formal} == orders
        assert warm[0]["order"] == warm[1]["order"][::-1]
        for r in warm + formal:
            assert set(r["engines"]) == set(engines)
            for engine, timings in r["engines"].items():
                assert timings["docs"] == 1_000_000
                assert timings["total_ns"] == sum(timings[k] for k in ("write_ns", "commit_ns", "close_ns"))
                wanted.add(f"b{batch}-{r['phase']}{r['round']}-{engine}")
    audits = protocol.read_rows(out / "audit.jsonl")
    assert Counter(a["label"].split("-")[0] for a in audits) == {"fixture": 4, "small": 4, "million": 9}
    assert len(list(out.glob("checkindex-*.log"))) == 7
    assert len(wanted) == 72
    assert Counter(r["label"] for r in protocol.read_rows(out / "indexes.jsonl")) == {w: 1 for w in wanted}
    verification = protocol.read_rows(out / "verification.jsonl")
    assert Counter(r["label"] for r in verification if r["label"].startswith("b")) == {
        f"{w}-{reader}": 1 for w in wanted for reader in ("java", "rust")
    }
    assert not any((out / name).exists() for name in wanted)
    assert rt.load(out / "PGO.json")["writes"] == receipt["preset"]["pgo_training_writes"] == 4
    assert len(protocol.read_rows(out / "training-samples.jsonl")) == 4


def run(args, preset):
    workload = preset["workload"]
    suite = SUITE / workload
    data_root = (args.data or rt.BENCHMARK / "data" / preset["dataset"]).expanduser().resolve()
    if args.jars_dir:
        args.jars_dir = args.jars_dir.expanduser().resolve()
    with rt.Lock():
        out = rt.new_run(suite, args.output, args.action)
        receipt = {"action": args.action, "status": "running", "preset": preset, "machine": rt.machine(),
                   "started": time.time(), "order_indices": protocol.ORDERS}
        rt.save(out / "RUN.json", receipt)
        try:
            commit = preset["source_commit"]
            source = freeze_source(out, commit)
            frozen_source = source_identity(source, commit)
            suite_inputs = rt.suite_inputs(suite)
            runner = {name: rt.sha(SUITE / name) for name in
                      ("structured.py", "protocol.py", "bench.py", f"presets/{preset['dataset']}.json")}
            driver = suite / "driver"
            shutil.copytree(driver, out / "driver")
            driver_files = rt.files(driver)
            receipt.update(source=frozen_source, suite_inputs=suite_inputs, runner=runner,
                           driver=driver_files, rustc=rt.output(["rustc", "+1.98.1", "-vV"]),
                           cargo=rt.output(["cargo", "+1.98.1", "-V"]))
            commands, jdk, jars = build(out, args, preset)
            receipt.update(jdk=jdk, jars=jars)
            rt.save(out / "RUN.json", receipt)
            if args.action != "build":
                prepare = importlib.import_module(workload.replace("-", "_"))
                fixture = out / "fixture"
                fixture.mkdir()
                prepare.write_fixture(fixture)
                preflight(commands, fixture, "fixture", out)
            if args.action == "run":
                data, manifest, archive = indexing_inputs.prepare(preset["dataset"], data_root)
                receipt.update(data=str(data), dataset=manifest, dataset_manifest_sha256=rt.sha(data / "MANIFEST.json"))
                rt.save(out / "RUN.json", receipt)
                preflight(commands, data, "small", out)
                commands["tuned"] = train(out, data, manifest, preset)
                expected = preflight(commands, data, "million", out)
                frozen_artifacts = artifacts(out)
                receipt.update(commands={e: list(map(str, c)) for e, c in commands.items()}, artifacts=frozen_artifacts)
                rt.save(out / "RUN.json", receipt)

                def verify(label, index):
                    for reader in ("java", "rust"):
                        reopen(commands[reader], index, data / "million-queries.json", expected, out, label + "-" + reader)
                    protocol.append(out / "indexes.jsonl", {"label": label, "files": rt.snapshot(index)})
                    shutil.rmtree(index)

                protocol.measure(commands, data, preset, out, verify)
                check(out, receipt)
                indexing_inputs.verify(data, {"derived": manifest["files"]})
                rt.require(rt.sha(archive) == manifest["snapshot"]["sha256"]
                           and rt.sha(data / "MANIFEST.json") == receipt["dataset_manifest_sha256"]
                           and artifacts(out) == frozen_artifacts, "Dataset or frozen programs changed")
            else:
                receipt.update(commands={e: list(map(str, c)) for e, c in commands.items()}, artifacts=artifacts(out))
            rt.require(source_identity(source, commit) == frozen_source and rt.suite_inputs(suite) == suite_inputs
                       and all(rt.sha(SUITE / path) == sha for path, sha in runner.items())
                       and rt.files(out / "driver") == driver_files and rt.jdk(args.java_home) == jdk
                       and all(rt.sha(p) == sha for p, sha in jars.items()), "Source, drivers, JVM or JAR changed")
            receipt.update(status="complete", finished=time.time())
            rt.save(out / "RUN.json", receipt)
            if args.action == "run":
                protocol.summarize(out)
        except BaseException as exc:
            receipt.update(status="stopped" if isinstance(exc, KeyboardInterrupt) else "failed",
                           error=repr(exc), finished=time.time())
            rt.save(out / "RUN.json", receipt)
            raise
        print("COMPLETE", out, flush=True)
