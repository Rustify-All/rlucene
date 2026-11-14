"""Run or inspect the latest recorded release's five indexing workloads."""
import argparse
import difflib
import fcntl
import json
import math
import os
from pathlib import Path
import re
import shutil
import signal
import statistics
import subprocess
import sys
import time

from common import runtime as rt

SUITE = Path(__file__).resolve().parent
METRICS = ("write_ns", "commit_ns", "close_ns", "total_ns")


def statistics_for(samples, engine):
    result = {}
    for metric in METRICS:
        ratios = [row["engines"][engine][metric] / row["engines"]["java"][metric] for row in samples]
        geometric = math.exp(statistics.mean(math.log(value) for value in ratios))
        rust = statistics.median(row["engines"][engine][metric] for row in samples)
        java = statistics.median(row["engines"]["java"][metric] for row in samples)
        result[metric] = {"candidate_median_ns": rust, "java_median_ns": java,
            "paired_median_time_change_pct": (statistics.median(ratios) - 1) * 100,
            "paired_geomean_time_change_pct": (geometric - 1) * 100,
            "median_throughput_change_pct": (java / rust - 1) * 100,
            "paired_geomean_throughput_change_pct": (1 / geometric - 1) * 100,
            "faster_pairs": sum(value < 1 for value in ratios), "pairs": len(ratios)}
    return result


def table(records):
    lines = ["| Dataset | Rust configuration | Java total (s) | Rust total (s) | Throughput change |",
             "|---|---|---:|---:|---:|"]
    for record in records:
        samples = record["samples"]
        engines = set(samples[0]["engines"]) - {"java"}
        preset = record["preset"]
        expected = {(batch, number) for batch in range(1, preset["batches"] + 1)
                    for number in range(1, preset["formal_per_batch"] + 1)}
        rt.require(len(samples) == len(expected)
                   and {(row["batch"], row["round"]) for row in samples} == expected,
                   "Missing or duplicated formal groups: " + record["dataset"])
        wanted = {"java", "rust"} if record["dataset"] == "wiki-medium-indexing-1m" else {"java", "rust", "tuned"}
        for row in samples:
            rt.require(set(row["engines"]) == set(row["order"]) == wanted
                       and len(row["order"]) == len(wanted), "Incomplete engine group")
            for values in row["engines"].values():
                rt.require(values.get("docs", values.get("documents")) == preset["documents"]
                           and all(values[key] > 0 for key in METRICS), "Invalid formal measurement")
        computed = {engine: statistics_for(samples, engine) for engine in sorted(engines)}
        if "reported" in record:
            for engine, metrics in computed.items():
                for metric, values in metrics.items():
                    for key, value in values.items():
                        rt.require(abs(value - record["reported"][engine][metric][key]) < 1e-6,
                                   "Saved reference statistics differ: " + record["dataset"])
        record["computed"] = computed
        for engine in ("rust", "tuned"):
            if engine not in computed:
                continue
            config = ("native + fat LTO; no PGO" if record["dataset"] == "wiki-medium-indexing-1m"
                      else "ordinary release" if engine == "rust" else "native + ThinLTO + PGO")
            values = computed[engine]["total_ns"]
            lines.append(f"| {record['dataset']} | {config} | {values['java_median_ns']/1e9:.4f} | "
                         f"{values['candidate_median_ns']/1e9:.4f} | {values['median_throughput_change_pct']:+.3f}% |")
    return "\n".join(lines)


def report(output):
    state = rt.load(output / "RELEASE_RUN.json")
    rt.require(state["status"] == "complete", "Release run is incomplete; retain its failure and partial results")
    records = []
    for entry in state["completed"]:
        run = Path(entry["run"])
        receipt = rt.load(run / "RUN.json")
        rt.require(receipt["status"] == "complete", "Incomplete dataset: " + entry["dataset"])
        if state["action"] == "release-run":
            samples = [json.loads(line) for line in (run / "samples.jsonl").read_text().splitlines()]
            samples = [row for row in samples if row["phase"] == "formal"]
            rt.require(len(samples) == receipt["preset"]["batches"] * receipt["preset"]["formal_per_batch"],
                       "Missing formal groups")
            records.append({"dataset": entry["dataset"], "run": str(run), "samples": samples,
                            "preset": receipt["preset"], "run_sha256": rt.sha(run / "RUN.json")})
    text = ("# " + state["release"] + " indexing\n\n" + table(records) + "\n" if records else
            "# Release smoke checks\n\nAll selected dataset preflights completed. No performance result was measured.\n")
    rt.save(output / "SUMMARY.json", {"release": state["release"], "commit": state["commit"],
                                      "action": state["action"], "checked_datasets": state["completed"],
                                      "datasets": records})
    (output / "SUMMARY.md").write_text(text)
    print(text)


def stage(output, release):
    import structured
    library = output / "library"
    library.mkdir()
    source = structured.freeze_source(library, release["commit"])
    proof = structured.source_identity(source, release["commit"])
    rt.save(output / "LIBRARY.json", {**proof, **rt.source_identity(source, release["commit"])})
    harness = output / "harness"
    shutil.copytree(rt.BENCHMARK, harness,
                    ignore=shutil.ignore_patterns(".cache", "data", "runs", "__pycache__", "target", "*.pyc"))
    package = (source / "Cargo.toml").read_text().split("[package]", 1)[1].split("\n[", 1)[0]
    selected_version = re.search(r'^version\s*=\s*"([^"]+)"', package, re.MULTILINE)
    rt.require(selected_version is not None, "Expected a literal package.version in the selected release")
    version = selected_version.group(1)
    rt.require(version == release["release"].removeprefix("v"), "Release tag and Cargo version differ")
    changes = []
    for lock in (harness / "indexing").rglob("Cargo.lock"):
        before = lock.read_text()
        after, count = re.subn(r'(\[\[package\]\]\nname = "rlucene"\nversion = ")[^"]+(\")',
                               lambda match: match[1] + version + match[2], before)
        rt.require(count == 1, "Expected one path-dependent rlucene lock entry: " + str(lock))
        lock.write_text(after)
        changes.extend(difflib.unified_diff(before.splitlines(True), after.splitlines(True),
                                           fromfile=str(lock.relative_to(harness)), tofile=str(lock.relative_to(harness))))
    for path in ("indexing/rust/Cargo.toml", "indexing/ms-marco-passage/rust/Cargo.toml"):
        manifest = harness / path
        before = manifest.read_text()
        after, count = re.subn(r'(rlucene\s*=\s*\{[^\n]*path\s*=\s*)"[^"]+"',
                               lambda match: match[1] + json.dumps(str(source)), before)
        rt.require(count == 1, "Missing path dependency: " + path)
        manifest.write_text(after)
        changes.extend(difflib.unified_diff(before.splitlines(True), after.splitlines(True), fromfile=path, tofile=path))
    (output / "STAGING.patch").write_text("".join(changes))
    rt.save(output / "HARNESS.json", rt.files(harness))
    return source, harness


def execute(args, release, datasets, data):
    rt.macos()
    selected = args.output or rt.BENCHMARK / "runs/indexing-release" / time.strftime("%Y%m%d-%H%M%S")
    output = rt.new_run(SUITE, selected, args.action)
    state = {"status": "waiting", "action": args.action, **release, "selected_datasets": datasets,
             "data": {key: str(value) for key, value in data.items()}, "completed": [], "started": time.time()}
    rt.save(output / "RELEASE_RUN.json", state)
    with rt.Lock() as lock:
        lock.stream.seek(0)
        lock.stream.truncate()
        lock.stream.write(json.dumps({"task": str(output), "pid": os.getpid(), "release": release["release"]}))
        lock.stream.flush()
        try:
            if "nyc-yellow-taxi-2025-01-1m" in datasets:
                try:
                    import numpy
                    import pyarrow
                except ImportError as error:
                    raise RuntimeError("Install benchmark/datasets/nyc-yellow-taxi/requirements.txt in this Python environment before building") from error
                rt.require(numpy.__version__ == "2.0.2" and pyarrow.__version__ == "21.0.0", "Taxi requires the pinned NumPy/PyArrow versions")
            state["status"] = "running"
            source, harness = stage(output, release)
            context = output / "CONTEXT.json"
            rt.save(context, {"source": str(source), "git_repository": str(rt.GIT_REPOSITORY),
                              "cache": str(output / ".cache"), "commit": release["commit"],
                              "lock_fd": lock.stream.fileno()})
            env = rt.environment()
            env.update(RLUCENE_BENCHMARK_CONTEXT=str(context), PYTHONDONTWRITEBYTECODE="1", CARGO_BUILD_JOBS="4")
            for dataset in datasets:
                common = ["--dataset", dataset, "--data", str(data[dataset])]
                for flag, value in (("--java-home", args.java_home), ("--jars-dir", args.jars_dir)):
                    if value:
                        common += [flag, str(value.resolve())]
                action = "run" if args.action == "release-run" else "smoke"
                commands = []
                if dataset == "rally-http-logs-1m":
                    commands.append(["build", *common])
                    if action == "run":
                        commands.append(["build-tuned", *common])
                run = output / dataset
                command = [action, *common, "--output", str(run)]
                if dataset in ("rally-http-logs-1m", "ms-marco-passage-1m"):
                    command += ["--rust-profile", "both" if action == "run" else "ordinary"]
                commands.append(command)
                for command in commands:
                    argv = [sys.executable, "-B", str(harness / "indexing/bench.py"), *command]
                    state.update(dataset=dataset, command=argv)
                    rt.save(output / "RELEASE_RUN.json", state)
                    print("EXEC", dataset, command[0], flush=True)
                    process = subprocess.Popen(argv, env=env, pass_fds=(lock.stream.fileno(),), start_new_session=True)
                    try:
                        rt.require(process.wait() == 0, "Dataset command failed; its logs and partial outputs were retained")
                    except BaseException:
                        if process.poll() is None:
                            try:
                                os.killpg(process.pid, signal.SIGINT)
                            except ProcessLookupError:
                                pass
                            process.wait()
                        raise
                rt.require(rt.load(run / "RUN.json")["status"] == "complete", "Incomplete dataset")
                state["completed"].append({"dataset": dataset, "run": str(run)})
            rt.require(not rt.source_identity(source, release["commit"])["modified_library_files"], "Release source changed")
            rt.require(rt.files(harness) == rt.load(output / "HARNESS.json"), "Frozen benchmark harness changed")
            state.update(status="complete", finished=time.time())
        except BaseException as error:
            state.update(status="stopped" if isinstance(error, KeyboardInterrupt) else "failed",
                         error=repr(error), finished=time.time())
            raise
        finally:
            rt.save(output / "RELEASE_RUN.json", state)
    report(output)


def main(argv=None):
    release = rt.load(SUITE / "release.json")
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("action", choices=("release-plan", "release-smoke", "release-run", "release-report", "release-reference"))
    parser.add_argument("--dataset", choices=["all", *release["datasets"]], default="all")
    parser.add_argument("--data-root", type=Path, default=rt.BENCHMARK / "data")
    parser.add_argument("--data", action="append", default=[], metavar="DATASET=DIR")
    parser.add_argument("--output", type=Path)
    parser.add_argument("--java-home", type=Path)
    parser.add_argument("--jars-dir", type=Path)
    args = parser.parse_args(argv)
    if args.action == "release-reference":
        reference = rt.load(SUITE / release["reference"])
        rt.require(reference["commit"] == release["commit"], "Reference and latest release differ")
        rt.require(reference["crate_sha256"] == release["crate_sha256"], "Reference package differs")
        for record in reference["datasets"]:
            rt.require(rt.sha(rt.BENCHMARK / record["input_manifest"]) == record["input_manifest_sha256"],
                       "Input manifest differs from the latest reference: " + record["dataset"])
        print(table(reference["datasets"]))
        print("All saved reference statistics reproduced from the formal samples.")
        return
    if args.action == "release-report":
        rt.require(args.output, "Specify --output for a completed release run")
        report(args.output.resolve())
        return
    commit = rt.output(["git", "-C", rt.GIT_REPOSITORY, "rev-parse", release["release"] + "^{commit}"])
    rt.require(commit == release["commit"], "Release tag is missing or points to another commit")
    datasets = release["datasets"] if args.dataset == "all" else [args.dataset]
    data = {name: (args.data_root / name).expanduser().resolve() for name in datasets}
    for value in args.data:
        name, separator, directory = value.partition("=")
        rt.require(separator and name in datasets and directory, "Use --data DATASET=DIR for a selected dataset")
        data[name] = Path(directory).expanduser().resolve()
    if args.action == "release-plan":
        print(json.dumps({**release, "selected_datasets": datasets, "data": {k: str(v) for k, v in data.items()},
                          "presets": {name: rt.load(SUITE / "presets" / (name + ".json")) for name in datasets},
                          "executes_builds_or_writes": False}, indent=2))
        return
    signal.signal(signal.SIGTERM, signal.default_int_handler)
    execute(args, release, datasets, data)


if __name__ == "__main__":
    main()
