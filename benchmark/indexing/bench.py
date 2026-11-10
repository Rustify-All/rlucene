#!/usr/bin/env python3
"""Reproduce the paired single-writer indexing benchmark on a selected dataset."""
import argparse
from contextlib import ExitStack, contextmanager
from itertools import permutations
import json
import os
from pathlib import Path
from statistics import median
import subprocess
import sys
import tempfile

SUITE = Path(__file__).resolve().parent
BENCHMARK = SUITE.parent
sys.path[:0] = [str(BENCHMARK), str(BENCHMARK / "datasets")]
from common import runtime as rt
from rally_http_logs import verify as verify_rally

PRESETS = SUITE / "presets"
CACHE = rt.CACHE / SUITE.name
BINARY = "rally-http-logs-verify"
MAIN_CLASS = "RallyMillion"
ENGINES = ("java", "rust", "tuned")
METRICS = ("write_ns", "commit_ns", "close_ns", "total_ns")
ORDERS = ((0, 5, 1, 3, 2, 4), (4, 2, 3, 1, 5, 0), (2, 1, 5, 0, 4, 3))


def append(path, value):
    with path.open("a", encoding="utf-8") as stream:
        stream.write(json.dumps(value, ensure_ascii=False) + "\n")


def read_rows(path):
    return [json.loads(line) for line in path.read_text().splitlines()]


def checked(command, log, env=None):
    with log.open("wb") as stream, log.with_suffix(log.suffix + ".err").open("wb") as errors:
        subprocess.run(list(map(str, command)), cwd=rt.REPOSITORY, env=env or rt.environment(),
                       stdout=stream, stderr=errors, check=True)


@contextmanager
def server(command, directory, label):
    process = rt.Server(list(map(str, command)), directory / (label + ".stderr"), timeout=300)
    transcript = directory / (label + ".jsonl")
    try:
        ready = json.loads(process.read())
        rt.require(ready["ready"] and ready["pid"] == process.process.pid, "Engine readiness mismatch")
        append(transcript, {"ready": ready, "command": list(map(str, command))})

        def ask(request):
            request = str(request)
            append(transcript, {"request": request})
            result = json.loads(process.ask(request)[0])
            append(transcript, {"result": result})
            return result

        yield ready, ask
        append(transcript, {"process": process.close()})
    except BaseException:
        process.abort()
        raise


def verified_input(args, preset):
    data = (args.data or BENCHMARK / "data" / args.dataset).expanduser().resolve()
    manifest = rt.load(BENCHMARK / preset["manifest"])
    verify_rally(data, manifest)
    return data, manifest


def write(ask, index, preset):
    result = ask(index)
    rt.require(result["docs"] == preset["documents"]
               and all(result[key] > 0 for key in METRICS)
               and result["total_ns"] == sum(result[k] for k in METRICS[:-1]),
               "Writer result mismatch: " + str(result))
    return result


def build_tuned(args, preset):
    with rt.Lock():
        ordinary = rt.programs(SUITE, args.java_home)
        data, manifest = verified_input(args, preset)
        out = Path(tempfile.mkdtemp(prefix="tuned-", dir=CACHE))
        profiles = out / "raw"
        profiles.mkdir()
        log = {"ok": False, "directory": str(out), "ordinary_build": ordinary,
               "preset": preset, "input_sha256": manifest["files"]["million.bin"]["sha256"],
               "machine": rt.machine(), "commands": []}
        try:
            env = rt.environment()
            env.update(CARGO_PROFILE_RELEASE_LTO="thin", CARGO_PROFILE_RELEASE_CODEGEN_UNITS="16")
            cargo = ["cargo", "+1.98.1", "build", "--locked", "--release", "-v",
                     "--manifest-path", SUITE / "rust/Cargo.toml", "--bin", BINARY]

            def build(name, flag):
                target = out / name
                command = [*cargo, "--target-dir", target]
                env["CARGO_ENCODED_RUSTFLAGS"] = "\x1f".join(("-Ctarget-cpu=native", flag))
                log["commands"].append({"command": list(map(str, command)), "rustflags": env["CARGO_ENCODED_RUSTFLAGS"].split("\x1f")})
                print("BUILD", name, out, flush=True)
                checked(command, out / (name + ".log"), env)
                return target / "release" / BINARY

            instrumented = build("instrumented", f"-Cprofile-generate={profiles}")
            training = [str(out / f"train-{n + 1}") for n in range(preset["pgo_training_writes"])]
            train = ["env", f"tests.seed={preset['seed']}", f"LLVM_PROFILE_FILE={profiles}/%p-%m.profraw",
                     instrumented, "reuse", data / "million.bin"]
            print("TRAIN", len(training), "million-document writes", flush=True)
            with server(train, out, "training") as (ready, ask):
                rt.require(ready["input_bytes"] == manifest["files"]["million.bin"]["bytes"], "Wrong training input size")
                for path in training:
                    write(ask, path, preset)

            profdata = Path(rt.output(["rustc", "+1.98.1", "--print", "target-libdir"])).parent / "bin/llvm-profdata"
            rt.require(profdata.is_file(), "Install llvm-tools-preview for Rust 1.98.1")
            raw = sorted(profiles.glob("*.profraw"))
            rt.require(raw, "PGO training produced no profile")
            merged = out / "rally.profdata"
            checked([profdata, "merge", "-o", merged, *raw], out / "merge.log")
            checked([profdata, "show", merged], out / "profile.txt")
            tuned = build("optimized", f"-Cprofile-use={merged}")
            rt.require(rt.programs(SUITE, args.java_home) == ordinary, "Ordinary build or source changed")
            log.update(ok=True, binary=str(tuned), binary_sha256=rt.sha(tuned), profile_sha256=rt.sha(merged),
                       training_writes=len(training), training_sha256=rt.sha(out / "training.jsonl"))
        finally:
            rt.save(out / "BUILD.json", log)
            if log["ok"]:
                rt.save(CACHE / "TUNED.json", log)
        print("BUILT", tuned, flush=True)


def oracle(data):
    expected = rt.load(data / "oracle.json")
    return {
        **expected["hits"],
        **{f"stable_{key}": [hit for page in pages for hit in page]
           for key, pages in expected["stable_pages"].items()},
        **{f"count_{key}": count for key, count in expected["counts"].items()},
    }


def verify_index(engine, command, index, data, expected, out, label, preset):
    with server([*command, "query", index, data / "input.json"], out, f"verify-{label}-{engine}") as (ready, ask):
        rt.require(ready["docs"] == preset["documents"] and ready.get("leaves", len(preset["segments"])) == len(preset["segments"]),
                   "Index geometry mismatch: " + str(index))
        rt.require(ask("verify") == expected, "Query oracle mismatch: " + str(index))
    append(out / "verification.jsonl", {"label": label, "reader": engine, "scenarios": len(expected)})


def preflight(commands, java, audit, data, expected, preset, out):
    indexes = {}
    for engine, command in commands.items():
        index = out / f"preflight-{engine}"
        indexes[engine] = index
        with server([*command, "reuse", data / "million.bin"], out, f"write-preflight-{engine}") as (_, ask):
            write(ask, index, preset)
        for reader, reader_command in commands.items():
            verify_index(reader, reader_command, index, data, expected, out, f"preflight-{engine}", preset)
        path = out / f"checkindex-{engine}.log"
        checked([*java[:-1], "org.apache.lucene.index.CheckIndex", index, "-level", "2"], path)
        rt.require("No problems were detected" in path.read_text(), "CheckIndex failed: " + str(path))
    audit_path = out / "audit.jsonl"
    checked([audit, data / "million.bin", indexes["java"], *indexes.values()], audit_path)
    audits = read_rows(audit_path)
    rt.require(len(audits) == len(indexes), "Incomplete full index audit")
    for entry, index in zip(audits, indexes.values()):
        rt.require(entry["index"] == str(index)
                   and entry["docs"] == entry["stored_documents"] == preset["documents"]
                   and entry["point_values"] == preset["documents"] * 4 and entry["doc_values_columns"] == 6
                   and entry["segments"] == preset["segments"] and entry["postings"] == preset["postings"],
                   "Full index audit mismatch: " + str(entry))
    print("PREFLIGHT OK", len(indexes), flush=True)


def measure(commands, data, expected, preset, out):
    orders = tuple(permutations(ENGINES))
    for batch, schedule in enumerate(ORDERS, 1):
        balanced = [tuple(e for e in orders[i] if e in commands) for i in schedule]
        written = []
        with ExitStack() as stack:
            writers = {engine: stack.enter_context(server([*command, "reuse", data / "million.bin"], out,
                       f"batch-{batch}-{engine}")) for engine, command in commands.items()}
            for phase, plan in (("warm", (balanced[0], balanced[0][::-1])), ("formal", balanced)):
                for number, order in enumerate(plan, 1):
                    results = {}
                    for engine in order:
                        label = f"b{batch}-{phase}{number}-{engine}"
                        index = out / label
                        ready, ask = writers[engine]
                        rt.require(ready["input_bytes"] == (data / "million.bin").stat().st_size, "Wrong input size")
                        results[engine] = write(ask, index, preset)
                        written.append((label, index))
                    append(out / "samples.jsonl", {"batch": batch, "phase": phase, "round": number,
                                                  "order": order, "engines": results})
                    print(batch, phase, number, {e: round(r["total_ns"] / 1e6, 3) for e, r in results.items()}, flush=True)
        for label, index in written:
            verify_index("java", commands["java"], index, data, expected, out, label, preset)


def summarize(out):
    receipt = rt.load(out / "RUN.json")
    rt.require(receipt["status"] == "complete" and receipt["action"] == "run", "A complete timed run is required")
    samples = [r for r in read_rows(out / "samples.jsonl") if r["phase"] == "formal"]
    preset = receipt["preset"]
    rt.require(len(samples) == preset["batches"] * preset["formal_per_batch"], "Incomplete samples")
    engines = tuple(receipt["commands"])

    def stats(block):
        result = {}
        for metric in METRICS:
            values = {e: [r["engines"][e][metric] for r in block] for e in engines}
            comparisons = {}
            for candidate, reference in (("rust", "java"), ("tuned", "java"), ("tuned", "rust")):
                if candidate in values and reference in values:
                    pairs = list(zip(values[candidate], values[reference]))
                    changes = [100 * (a / b - 1) for a, b in pairs]
                    comparisons[f"{candidate}_vs_{reference}"] = {
                        "median_change_pct": median(changes), "faster_rounds": sum(a < b for a, b in pairs),
                        "rounds": len(pairs), "paired_changes_pct": changes,
                    }
            result[metric] = {"medians_ns": {e: median(v) for e, v in values.items()}, "comparisons": comparisons}
        return result

    result = {"overall": stats(samples), "batches": {str(b): stats([r for r in samples if r["batch"] == b])
              for b in range(1, preset["batches"] + 1)}}
    rt.save(out / "RESULT.json", result)
    columns = tuple(result["overall"]["total_ns"]["comparisons"])
    table = ["# " + preset["dataset"] + " index writing", "",
             "Per-engine median in milliseconds. Changes are medians of matched time ratios minus one.", "",
             "| Phase | " + " | ".join(engines + columns) + " |",
             "| --- | " + " | ".join("---:" for _ in engines + columns) + " |"]
    for metric, row in result["overall"].items():
        values = [f"{row['medians_ns'][e] / 1e6:.3f}" for e in engines]
        values += [f"{row['comparisons'][c]['median_change_pct']:+.2f}%" for c in columns]
        table.append("| " + " | ".join([metric.removesuffix("_ns"), *values]) + " |")
    table.extend(["", "[Per-batch results](RESULT.json) · [Raw samples](samples.jsonl) · [Run identity](RUN.json)", ""])
    (out / "REPORT.md").write_text("\n".join(table), encoding="utf-8")


def run(args, preset):
    with rt.Lock():
        data, manifest = verified_input(args, preset)
        build = rt.programs(SUITE, args.java_home)
        base = rt.commands(SUITE, {"java": [], "rust": []}, args.java_home)
        java = [base["java"][0], f"-Dtests.seed={preset['seed']}", *base["java"][1:]]
        commands = {"java": java}
        if args.rust_profile in ("ordinary", "both"):
            commands["rust"] = ["env", f"tests.seed={preset['seed']}", *base["rust"]]
        tuned = None
        if args.rust_profile in ("tuned", "both"):
            tuned = rt.load(CACHE / "TUNED.json")
            rt.require(tuned["ok"] and tuned["ordinary_build"] == build and tuned["preset"] == preset
                       and tuned["machine"] == rt.machine()
                       and tuned["input_sha256"] == manifest["files"]["million.bin"]["sha256"]
                       and tuned["binary_sha256"] == rt.sha(tuned["binary"])
                       and tuned["profile_sha256"] == rt.sha(Path(tuned["directory"]) / "rally.profdata"),
                       "Tuned build, machine or input changed; run build-tuned again")
            commands["tuned"] = ["env", f"tests.seed={preset['seed']}", tuned["binary"]]
        audit = CACHE / "target/release/audit"
        out = rt.new_run(SUITE, args.output, args.action)
        receipt = {"action": args.action, "status": "running", "preset": preset, "dataset": manifest,
                   "data": str(data), "machine": rt.machine(), "build": build, "tuned_build": tuned,
                   "commands": commands, "audit_sha256": rt.sha(audit), "order_indices": ORDERS}
        rt.save(out / "RUN.json", receipt)
        try:
            expected = oracle(data)
            preflight(commands, java, audit, data, expected, preset, out)
            if args.action == "run":
                measure(commands, data, expected, preset, out)
            rt.require(rt.programs(SUITE, args.java_home) == build, "Build or source changed during run")
            verify_rally(data, manifest)
            rt.require(rt.sha(audit) == receipt["audit_sha256"], "Audit program changed during run")
            if tuned:
                rt.require(rt.sha(tuned["binary"]) == tuned["binary_sha256"], "Tuned program changed during run")
            receipt["status"] = "complete"
        except BaseException as exc:
            receipt.update(status="stopped" if isinstance(exc, KeyboardInterrupt) else "failed", error=repr(exc))
            raise
        finally:
            rt.save(out / "RUN.json", receipt)
        if args.action == "run":
            summarize(out)
        print("COMPLETE", out, flush=True)


def main():
    presets = {p.stem: p for p in sorted(PRESETS.glob("*.json"))}
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("action", choices=("datasets", "build", "build-tuned", "smoke", "run", "summarize"))
    parser.add_argument("--dataset", choices=presets, default="rally-http-logs-1m")
    parser.add_argument("--data", type=Path)
    parser.add_argument("--output", type=Path)
    parser.add_argument("--java-home", type=Path)
    parser.add_argument("--jars-dir", type=Path)
    parser.add_argument("--rust-profile", choices=("ordinary", "tuned", "both"), default="ordinary")
    args = parser.parse_args()
    if args.action == "datasets":
        for name, path in presets.items():
            print(name, rt.load(path)["documents"], "documents")
        return
    if args.action == "summarize":
        rt.require(args.output, "Specify --output for the completed run")
        summarize(args.output)
        return
    rt.macos()
    preset = rt.load(presets[args.dataset])
    if args.action == "build":
        rt.build(SUITE, BINARY, MAIN_CLASS, args.java_home, args.jars_dir)
    elif args.action == "build-tuned":
        build_tuned(args, preset)
    else:
        run(args, preset)


if __name__ == "__main__":
    main()
