#!/usr/bin/env python3
"""Reproduce the paired single-writer indexing benchmark on a selected dataset."""
import argparse
from pathlib import Path
import sys
import tempfile

SUITE = Path(__file__).resolve().parent
BENCHMARK = SUITE.parent
sys.path[:0] = [str(BENCHMARK), str(BENCHMARK / "datasets")]
from common import runtime as rt
from rally_http_logs import verify as verify_rally
from protocol import ORDERS, append, checked, measure, read_rows, server, summarize, write

PRESETS = SUITE / "presets"
CACHE = rt.CACHE / SUITE.name
BINARY = "rally-http-logs-verify"
MAIN_CLASS = "RallyMillion"


def verified_input(args, preset):
    data = (args.data or BENCHMARK / "data" / args.dataset).expanduser().resolve()
    manifest = rt.load(BENCHMARK / preset["manifest"])
    verify_rally(data, manifest)
    return data, manifest


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
                measure(commands, data, preset, out, lambda label, index: verify_index(
                    "java", commands["java"], index, data, expected, out, label, preset))
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
    parser.add_argument("--rust-profile", choices=("ordinary", "tuned", "both"))
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
    if preset.get("runner") == "wiki_medium":
        from wiki_medium import run as run_wiki_medium
        if args.action == "build-tuned" or args.rust_profile is not None:
            parser.error("Wiki medium uses its pinned native CPU + fat LTO builds")
        run_wiki_medium(args, preset)
        return
    if preset.get("runner") == "structured":
        from structured import run as run_structured
        if args.action == "build-tuned":
            parser.error("Use run to train and verify the optimized build in the original three-engine protocol")
        profile = "both" if args.action == "run" else "ordinary"
        if args.rust_profile not in (None, profile):
            parser.error(f"{args.action} for {args.dataset} requires --rust-profile {profile}")
        run_structured(args, preset)
        return
    args.rust_profile = args.rust_profile or "ordinary"
    if args.dataset == "ms-marco-passage-1m":
        from ms_marco import run as run_ms_marco
        if args.action == "build-tuned":
            parser.error("For MS MARCO, use smoke or run with --rust-profile both to build and validate PGO")
        run_ms_marco(args, preset)
    elif args.action == "build":
        rt.build(SUITE, BINARY, MAIN_CLASS, args.java_home, args.jars_dir)
    elif args.action == "build-tuned":
        build_tuned(args, preset)
    else:
        run(args, preset)


if __name__ == "__main__":
    main()
