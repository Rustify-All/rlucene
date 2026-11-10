"""Reproduce the Wiki medium million-document, single-segment comparison."""
from contextlib import ExitStack, contextmanager
import hashlib
import json
import os
from pathlib import Path
import shutil

from common import runtime as rt
import protocol
from structured import checked, freeze_source, source_identity

SUITE = Path(__file__).resolve().parent
WHITE_SPACE = "".join(chr(value) for value in (
    *range(9, 14), *range(28, 33), 0x1680, *range(0x2000, 0x2007),
    *range(0x2008, 0x200B), 0x2028, 0x2029, 0x205F, 0x3000,
))


def verify_input(corpus, preset, manifest):
    sha = hashlib.sha256()
    units = count = 0
    expected = preset["utf16_units"]
    with corpus.open("rb") as stream:
        for count, raw in enumerate(stream, 1):
            sha.update(raw)
            title, _, body, label, *_ = raw.decode("utf-8").split("\t", 4)
            units += len((title + body + label.strip(WHITE_SPACE)).encode("utf-16-le")) // 2
            if str(count) in expected:
                rt.require(units == expected[str(count)], f"UTF-16 count mismatch at row {count}")
    rt.require(count == preset["documents"] and corpus.stat().st_size == manifest["corpus"]["bytes"]
               and sha.hexdigest() == manifest["corpus"]["sha256"], "Wiki medium corpus mismatch")


def build_rust(out, name, commit):
    snapshot = out / name
    snapshot.mkdir()
    source = freeze_source(snapshot, commit)
    identity = source_identity(source, commit)
    staged = out / "source"
    source.rename(staged)
    try:
        # The second snapshot must invalidate Cargo even when its archive mtimes
        # predate the first build.
        for path in staged.rglob("*"):
            if path.is_file() and not path.is_symlink():
                os.utime(path, None)
        env = rt.environment()
        env.update(RUSTFLAGS="-C target-cpu=native", CARGO_PROFILE_RELEASE_LTO="fat", CARGO_BUILD_JOBS="4")
        log = out / f"build-{name}.log"
        print("BUILD", name, commit, flush=True)
        checked(["cargo", "+1.98.1", "build", "--release", "--locked", "--verbose", "--bin",
                 "indexing-benchmark", "--manifest-path", out / "driver/Cargo.toml",
                 "--target-dir", out / "target"], log, env, out)
        lines = log.read_text().splitlines() + log.with_suffix(".log.err").read_text().splitlines()
        [consumer] = [line for line in lines if "--crate-name indexing_benchmark " in line]
        [library] = [line for line in lines if "--crate-name rlucene " in line]
        rt.require("-C lto=fat" in consumer and "target-cpu=native" in consumer and
                   "target-cpu=native" in library, "Unexpected Rust release settings")
        metadata = consumer.split("-C metadata=", 1)[1].split()[0]
        binary = snapshot / "indexing-benchmark"
        shutil.copy2(out / "target/release/indexing-benchmark", binary)
    finally:
        staged.rename(source)
    rt.require(source_identity(source, commit) == identity, "Frozen Rust sources changed")
    return str(binary), {**identity, "consumer_metadata": metadata, "binary_sha256": rt.sha(binary)}


def build(out, args, preset):
    driver = out / "driver"
    shutil.copytree(SUITE / "wiki-medium/driver", driver)
    hashes = rt.files(driver)
    java = rt.jdk(args.java_home)
    jars_dir = rt.dependencies(args.jars_dir)
    jars = [jars_dir / item["file"] for item in rt.load(rt.BENCHMARK / "common/java-jars.json")]
    classes = out / "classes"
    classes.mkdir()
    cp = os.pathsep.join(map(str, jars))
    home = Path(java["home"])
    checked([home / "bin/javac", "--release", "23", "-encoding", "UTF-8", "-cp", cp, "-d", classes,
             *sorted((driver / "java").glob("*.java"))], out / "javac.log")
    old, old_build = build_rust(out, "old", preset["old_commit"])
    rust, rust_build = build_rust(out, "rust", preset["rust_commit"])
    rt.require(old_build["consumer_metadata"] == rust_build["consumer_metadata"],
               "Rust consumer metadata differs between builds")
    commands = {"java": [str(home / "bin/java"), *preset["java_flags"], "-cp", f"{classes}{os.pathsep}{cp}"],
                "old": [old], "rust": [rust]}
    identity = {"java": java, "jars": {str(jar): rt.sha(jar) for jar in jars}, "classes": rt.files(classes),
                "driver": hashes, "rustc": rt.output(["rustc", "+1.98.1", "-Vv"]),
                "old": old_build, "rust": rust_build}
    rt.require(rt.files(driver) == hashes, "Benchmark driver changed during build")
    return commands, identity


@contextmanager
def writer(out, name, command):
    process = rt.Server(command, out / f"{name}.stderr", timeout=600)
    try:
        yield process
        protocol.append(out / "processes.jsonl", {"name": name, "pid": process.process.pid, **process.close()})
    except BaseException:
        process.abort()
        raise


def sample(out, label, engine, process, commands, corpus, preset, count):
    folder = out / label
    folder.mkdir()
    index = folder / "index"
    config = {**preset["writer"], "action": "index", "corpus": str(corpus),
              "documents": count, "index": str(index), "warmup": 10000, "run_count": 100000}
    request = folder / "write.json"
    rt.save(request, config)
    print("WRITE", label, count, "documents", flush=True)
    response, _ = process.ask(str(request))
    (folder / "write.stdout").write_text(response + "\n", encoding="utf-8")
    metrics = json.loads(response)
    expected = preset["utf16_units"][str(count)] if engine == "java" else 0
    rt.require(metrics["documents"] == count and metrics["persisted"] and metrics["rollback_ns"] == 0
               and metrics["luceneutil_utf16_units"] == expected, f"Invalid write response for {label}")
    metrics["close_ns"] = metrics["close_wait_merges_ns"]
    rt.require(all(metrics[key] > 0 for key in protocol.METRICS), f"Invalid timing for {label}")
    protocol.append(out / "measured-before-validation.jsonl", {"label": label, "engine": engine, "metrics": metrics})
    verify = folder / "verify.json"
    rt.save(verify, dict(config, action="verify"))
    checked([*commands["java"], "IndexingBench", verify], folder / "verify.stdout")
    logical = rt.load(folder / "verify.stdout")
    rt.require(logical["check_index"] and logical["num_docs"] == count and logical["segment_documents"] == [count],
               f"Index verification failed for {label}")
    if count == preset["documents"]:
        rt.require(logical["logical_sha256"] == preset["logical_sha256"], f"Logical hash mismatch for {label}")
    reopen = folder / "reopen.json"
    rt.save(reopen, dict(config, action="read"))
    reader = "old" if engine == "java" else engine
    checked([*commands[reader], reopen], folder / "reopen.stdout")
    rt.require(rt.load(folder / "reopen.stdout") == {"num_docs": count, "max_doc": count}, f"Reopen failed for {label}")
    protocol.append(out / "verification.jsonl", {
        "label": label, "engine": engine, "pid": process.process.pid, **logical,
        "index_bytes": sum(path.stat().st_size for path in index.iterdir() if path.is_file()),
    })
    print("VERIFIED", label, f"{metrics['total_ns'] / 1e9:.3f}s", flush=True)
    return metrics, logical


def writers(stack, out, prefix, commands):
    return {engine: stack.enter_context(writer(out, f"{prefix}-{engine}",
            [*command, "BenchmarkServer" if engine == "java" else "--server"]))
            for engine, command in commands.items()}


def measure(out, commands, corpus, preset):
    count = preset["documents"]
    for batch, schedule in enumerate(preset["orders"], 1):
        with ExitStack() as stack:
            engines = writers(stack, out, f"batch-{batch}", commands)
            elapsed = dict.fromkeys(engines, 0)
            threshold = preset["warmup_min_seconds"] * 10**9
            number = 0
            while min(elapsed.values()) < threshold:
                number += 1
                order = schedule[0] if number % 2 else schedule[0][::-1]
                results = {}
                for engine in order:
                    if elapsed[engine] < threshold:
                        results[engine], _ = sample(out, f"b{batch}-warm{number}-{engine}", engine,
                                                    engines[engine], commands, corpus, preset, count)
                        elapsed[engine] += results[engine]["total_ns"]
                protocol.append(out / "samples.jsonl", {"batch": batch, "round": number, "phase": "warm",
                                                       "order": list(results), "engines": results})
            protocol.append(out / "warmup-proof.jsonl", {
                "batch": batch, "elapsed_ns": elapsed, "pids": {e: p.process.pid for e, p in engines.items()},
            })
            for number, order in enumerate(schedule, 1):
                results = {e: sample(out, f"b{batch}-formal{number}-{e}", e, engines[e],
                                     commands, corpus, preset, count)[0] for e in order}
                protocol.append(out / "samples.jsonl", {"batch": batch, "round": number, "phase": "formal",
                                                       "order": order, "engines": results})


def run(args, preset):
    manifest = rt.load(rt.BENCHMARK / preset["manifest"])
    corpus = ((args.data or rt.BENCHMARK / "data" / args.dataset).expanduser().resolve()
              / manifest["corpus"]["file"])
    with rt.Lock():
        if args.action != "build":
            verify_input(corpus, preset, manifest)
        out = rt.new_run(SUITE, args.output, f"{args.dataset}-{args.action}")
        receipt = {"action": args.action, "status": "running", "preset": preset, "dataset": manifest,
                   "data": str(corpus), "machine": rt.machine()}
        try:
            commands, identity = build(out, args, preset)
            receipt.update(commands=commands, build=identity)
            rt.save(out / "RUN.json", receipt)
            if args.action in ("smoke", "run"):
                with ExitStack() as stack:
                    engines = writers(stack, out, "preflight", commands)
                    checks = [sample(out, f"preflight-{e}", e, p, commands, corpus,
                                     preset, preset["preflight_documents"])[1] for e, p in engines.items()]
                rt.require(all(check == checks[0] for check in checks), "Preflight indexes differ")
                if args.action == "run":
                    measure(out, commands, corpus, preset)
                rt.require(rt.sha(corpus) == manifest["corpus"]["sha256"], "Wiki medium corpus changed")
            for engine in ("old", "rust"):
                rt.require(rt.sha(commands[engine][0]) == identity[engine]["binary_sha256"], "Rust binary changed")
            receipt["status"] = "complete"
        except BaseException as exc:
            receipt.update(status="stopped" if isinstance(exc, KeyboardInterrupt) else "failed", error=repr(exc))
            raise
        finally:
            rt.save(out / "RUN.json", receipt)
        if args.action == "run":
            protocol.summarize(out)
        print("COMPLETE", out, flush=True)
