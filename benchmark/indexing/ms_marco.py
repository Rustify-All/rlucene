"""MS MARCO passage indexing and cross-engine correctness."""
import os
from pathlib import Path
import shutil
import time

from common import runtime as rt
from ms_marco_passage import FIXTURES, verify as verify_dataset
import protocol

SUITE = Path(__file__).resolve().parent / "ms-marco-passage"
CACHE = rt.CACHE / SUITE.name
BINARY = "ms-marco-passage-verify"


def checked(command, log, env=None):
    protocol.append(log.parent / "commands.jsonl", {"log": log.name, "argv": list(map(str, command))})
    protocol.checked(command, log, env)


def read_result(command, log):
    checked(command, log)
    return rt.load(log)


def identity():
    suite_inputs = rt.suite_inputs(SUITE)
    for path in (Path(__file__), Path(protocol.__file__), SUITE.parent / "bench.py",
                 SUITE.parent / "presets/ms-marco-passage-1m.json"):
        suite_inputs[path.relative_to(rt.BENCHMARK).as_posix()] = rt.sha(path)
    return {"source": rt.source_identity(), "suite_inputs": suite_inputs}


def cargo_build(out, name, target, rustflags=()):
    command = ["cargo", "+1.98.1", "build", "--locked", "--release", "-v",
               "--manifest-path", SUITE / "rust/Cargo.toml", "--target-dir", target]
    env = rt.environment()
    if rustflags:
        env.update(CARGO_PROFILE_RELEASE_LTO="thin", CARGO_PROFILE_RELEASE_CODEGEN_UNITS="16",
                   CARGO_ENCODED_RUSTFLAGS="\x1f".join(("-Ctarget-cpu=native", *rustflags)))
    protocol.append(out / "build-settings.jsonl", {"build": name, "flags": rustflags, "target": str(target),
        "lto": "thin" if rustflags else "default", "codegen_units": 16 if rustflags else "default"})
    print("BUILD", name, flush=True)
    checked(command, out / (name + ".log"), env)
    binary = out / name
    shutil.copy2(target / "release" / BINARY, binary)
    return binary


def build(out, args, preset):
    jdk = rt.jdk(args.java_home)
    jars = rt.dependencies(args.jars_dir)
    classes = out / "classes"
    classes.mkdir()
    jars_classpath = os.pathsep.join(map(str, sorted(jars.glob("*.jar"))))
    jdk_path = Path(jdk["home"])
    java = [jdk_path / "bin/java", "-Xms4g", "-Xmx4g", "-XX:+UseParallelGC",
            f"-Dtests.seed={preset['seed']}", "-cp", f"{classes}{os.pathsep}{jars_classpath}", "Passage"]
    checked([jdk_path / "bin/javac", "--release", "23", "-encoding", "UTF-8", "-cp", jars_classpath,
             "-d", classes, SUITE / "java/Passage.java"], out / "javac.log")
    rust = cargo_build(out, "ordinary", CACHE / "target")
    return {"java": java, "rust": ["env", f"tests.seed={preset['seed']}", rust]}, jars, jdk


def train(out, data, preset, manifest):
    profiles = out / "profiles"
    profiles.mkdir()
    build_profiles = out / "build-script-profiles"
    build_profiles.mkdir()
    instrumented = cargo_build(out, "instrumented", CACHE / "target-instrumented",
                               (f"-Cprofile-generate={build_profiles}",))
    binary = data / "million.bin"
    train = ["env", f"tests.seed={preset['seed']}", f"LLVM_PROFILE_FILE={profiles}/%p-%m.profraw",
             instrumented, "reuse", binary]
    started = time.monotonic()
    with protocol.server(train, out, "training") as (ready, ask):
        rt.require(ready["input_bytes"] == binary.stat().st_size, "Wrong PGO input size")
        for n in range(1, preset["pgo_training_writes"] + 1):
            index = out / f"training-index-{n}"
            sample = protocol.write(ask, index, preset)
            protocol.append(out / "training-samples.jsonl", {"write": n, "pid": ready["pid"], **sample})
            print("TRAIN", n, round(sample["total_ns"] / 1e9, 3), flush=True)
    elapsed = time.monotonic() - started
    raw = sorted(profiles.glob(f"{ready['pid']}-*.profraw"))
    rt.require(raw and len(raw) == len(list(profiles.glob("*.profraw"))),
               "PGO input must come only from the workload process")
    profdata = Path(rt.output(["rustc", "+1.98.1", "--print", "target-libdir"])).parent / "bin/llvm-profdata"
    rt.require(profdata.is_file(), "Install llvm-tools-preview for Rust 1.98.1")
    merged = profiles / "ms-marco.profdata"
    checked([profdata, "merge", "-o", merged, *raw], out / "pgo-merge.log")
    checked([profdata, "show", merged], out / "pgo-profile.log")
    rt.save(out / "PGO.json", {"wall_seconds": elapsed, "writes": preset["pgo_training_writes"],
        "input_sha256": manifest["files"]["million.bin"]["sha256"], "workload_pid": ready["pid"],
        "profile_files": rt.files(profiles), "llvm_profdata_sha256": rt.sha(profdata)})
    for n in range(1, preset["pgo_training_writes"] + 1):
        shutil.rmtree(out / f"training-index-{n}")
    tuned = cargo_build(out, "optimized", CACHE / "target-optimized", (f"-Cprofile-use={merged}",))
    return ["env", f"tests.seed={preset['seed']}", tuned]


def segments(count, preset):
    return preset["segments"] if count == preset["documents"] else [count]


def compare(expected, found, label, out):
    # Require exact geometry, counts, and ordered docIDs and pids. Scores are compared as f32 bits.
    rt.require((expected["docs"], expected["live"], expected["segments"]) ==
               (found["docs"], found["live"], found["segments"]), f"{label}: geometry mismatch")
    rt.require(len(expected["results"]) == len(found["results"]), f"{label}: query count mismatch")
    for a, b in zip(expected["results"], found["results"]):
        if a != b:
            protocol.append(out / "query-differences.jsonl", {"label": label, "expected": a, "observed": b})
    rt.require(expected == found, f"{label}: query mismatch. See query-differences.jsonl.")


def verify(command, index, cases, count, preset, out, label, expected=None):
    observed = read_result([*command, "query", index, cases], out / f"query-{label}.json")
    rt.require(observed["docs"] == observed["live"] == count and observed["segments"] == segments(count, preset),
               f"{label}: incorrect index size or segment geometry")
    if expected is not None:
        compare(expected, observed, label, out)
    protocol.append(out / "verification.jsonl", {"label": label, "docs": count, "queries": len(observed["results"]),
        "hits": sum(len(x[2]) for x in observed["results"]), "bits": "exact",
        "sha256": rt.sha(out / f"query-{label}.json")})
    return observed


def check_fixture(expected):
    count = {name: n for name, n, _ in expected["results"]}
    answers = {"pid-first": 1, "pid-missing": 0, "pid-unicode": 1, "pid-empty": 1, "fixture-term": 4,
               "fixture-phrase": 2, "fixture-cjk": 1, "fixture-tie": 2}
    rt.require(all(count[name] == n for name, n in answers.items()), "Independent fixture counts differ")
    hits = {name: [row[0] for row in rows] for name, _, rows in expected["results"]}
    rt.require(hits["fixture-tie"] == [12, 13] and set(hits["fixture-phrase"]) == {0, 1},
               "Independent fixture ordering or phrase membership differs")


def token_check(commands, input_path, out, label):
    results = {engine: read_result([*command, "tokens", input_path], out / f"tokens-{label}-{engine}.json")
               for engine, command in commands.items()}
    for engine, value in results.items():
        if value != results["java"]:
            diff = [i for i, (a, b) in enumerate(zip(results["java"], value)) if a != b]
            rt.save(out / f"tokens-{label}-{engine}.diff.json", {"mismatched_rows": diff})
            raise ValueError(f"{label}: analyzer token, position increment or UTF-16 offset mismatch in {diff}")
    print("TOKENS OK", label, len(results["java"]), flush=True)


def preflight(commands, data, stem, count, preset, out):
    binary = data / f"{stem}.bin"
    queries = data / f"{stem}-queries.json"
    indexes = {}
    expected = None
    java = commands["java"]
    audit = None
    for engine, command in commands.items():
        index = out / f"preflight-{stem}-{engine}"
        indexes[engine] = index
        with protocol.server([*command, "reuse", binary], out, f"write-{stem}-{engine}") as (ready, ask):
            rt.require(ready["input_bytes"] == binary.stat().st_size, f"Wrong input size: {binary}")
            protocol.write(ask, index, {"documents": count})
        check = out / f"checkindex-{stem}-{engine}.log"
        checked([*java[:-1], "org.apache.lucene.index.CheckIndex", index, "-level", "2"], check)
        rt.require("No problems were detected" in check.read_text(), f"CheckIndex failed: {index}")
        observed = read_result([*java, "audit", index, binary], out / f"audit-{stem}-{engine}.json")
        rt.require(observed["stored_documents"] == count
                   and [s["docs"] for s in observed["segments"]] == segments(count, preset), f"Audit geometry failed: {index}")
        if audit is None:
            audit = observed
        else:
            rt.require(audit == observed, f"{stem} index logical audit differs: {index}")
        for name, reader in commands.items():
            outcome = verify(reader, index, queries, count, preset, out, f"{stem}-{engine}-reader-{name}", expected)
            if expected is None:
                expected = outcome
    if stem == "fixture":
        check_fixture(expected)
    print("PREFLIGHT OK", stem, count, len(indexes), flush=True)
    return expected


def artifacts(out):
    names = ("ordinary", "optimized", "instrumented")
    return {"binaries": {name: rt.sha(out / name) for name in names if (out / name).exists()},
            "java_classes": rt.files(out / "classes"),
            "profiles": rt.files(out / "profiles") if (out / "profiles").exists() else {}}


def run(args, preset):
    if args.action == "build":
        rt.build(SUITE, BINARY, "Passage", args.java_home, args.jars_dir)
        return
    data = (args.data or rt.BENCHMARK / "data" / preset["dataset"]).expanduser().resolve()
    out = rt.new_run(SUITE.parent, args.output, args.action)
    receipt = {"action": args.action, "status": "waiting", "preset": preset, "data": str(data),
               "rust_profile": args.rust_profile, "order_indices": protocol.ORDERS, "machine": rt.machine()}
    rt.save(out / "RUN.json", receipt)
    with rt.Lock():
        try:
            receipt.update(status="running", lock_pid=os.getpid(), started=time.time())
            manifest = rt.load(rt.BENCHMARK / preset["manifest"])
            verify_dataset(data, manifest)
            source = identity()
            receipt.update(dataset=manifest, **source, rustc=rt.output(["rustc", "+1.98.1", "-vV"]),
                           cargo=rt.output(["cargo", "+1.98.1", "-V"]))
            commands, jars, jdk = build(out, args, preset)
            jar_identity = rt.files(jars)
            receipt.update(jdk=jdk, jars=jar_identity)
            rt.save(out / "RUN.json", receipt)
            token_check(commands, data / "fixture-tokens.json", out, "fixture")
            preflight(commands, data, "fixture", len(FIXTURES), preset, out)
            token_check(commands, data / "small-tokens.json", out, "small")
            preflight(commands, data, "small", preset["small_documents"], preset, out)
            if args.rust_profile in ("both", "tuned"):
                commands["tuned"] = train(out, data, preset, manifest)
            if args.rust_profile == "tuned":
                del commands["rust"]
            frozen = artifacts(out)
            receipt.update(commands={e: list(map(str, c)) for e, c in commands.items()}, artifacts=frozen)
            rt.save(out / "RUN.json", receipt)
            if args.action == "run" or "tuned" in commands:
                expected = preflight(commands, data, "million", preset["documents"], preset, out)
                rt.save(out / "query-oracle.json", expected)
            if args.action == "run":
                def reopen(label, index):
                    verify(commands["java"], index, data / "million-queries.json", preset["documents"],
                           preset, out, label, expected)
                    shutil.rmtree(index)

                protocol.measure(commands, data, preset, out, reopen)
            verify_dataset(data, manifest)
            rt.require(artifacts(out) == frozen and identity() == source
                       and rt.files(jars) == jar_identity and rt.jdk(args.java_home) == jdk,
                       "Source, programs, profiles or Java runtime changed")
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
