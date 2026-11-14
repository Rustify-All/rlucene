"""Lucene 10.1 / RLucene SIFT1M query comparison adapted from luceneutil."""
import argparse
import csv
import json
from pathlib import Path
import statistics
import struct
import subprocess
import sys
import time
import traceback

SUITE = Path(__file__).resolve().parent
sys.path.insert(0, str(SUITE.parent))
from common import runtime as rt

CONFIG = rt.load(SUITE / "configs/sift1m-top100.json")
NQ, K = CONFIG["queries"], CONFIG["top_k"]
INDEX_CONFIG = {key: CONFIG[key] for key in ["documents", "dimensions", "similarity", "encoding", "field", "M", "beam_width", "seed", "segments"]}


def input_identity(args):
    expected = rt.load(SUITE.parent / "datasets/sift1m/manifest.json")["files"]
    result = {}
    for name in ["query.raw", "groundtruth.raw"]:
        path = args.data / name
        rt.require(path.stat().st_size == expected[name]["bytes"] and rt.sha(path) == expected[name]["sha256"], "Wrong SIFT input: " + str(path))
        result[name] = expected[name]
    result["index"] = rt.files(args.index, exclude=("write.lock",))
    result["published_index_match"] = result["index"] == rt.load(SUITE.parent / "datasets/sift1m/published-index.json")
    result["index_parameter_provenance"] = "published-index-sha256" if result["published_index_match"] else "unverified-external-index"
    setup_path = args.index.parent / (args.index.name + "-manifest.json")
    if not result["published_index_match"] and setup_path.is_file():
        setup = rt.load(setup_path)
        if setup.get("index_files") == result["index"] and setup.get("index_config") == INDEX_CONFIG:
            result["index_parameter_provenance"] = "setup-receipt"
            result["index_setup_receipt_sha256"] = rt.sha(setup_path)
    return result


def launch(args, output, label):
    commands = rt.commands(SUITE, {"rust": [args.index, args.data / "query.raw", output],
                                  "java": ["server", args.index, args.data / "query.raw", output]},
                           args.java_home, heap=("512m", "6g"), gc=False)
    servers = {}
    try:
        for engine in rt.ENGINES:
            server = rt.Server(commands[engine], output / (label + "-" + engine + "-stderr.log"), timeout=300)
            servers[engine] = server
            rt.require(server.read() == "READY", "Missing vector READY")
    except BaseException:
        for server in servers.values():
            server.abort()
        raise
    return servers, commands


def quality(output, data):
    reference = (output / "check-java.hits").read_bytes()
    for name in ["check-rust.hits", "visit-rust.hits", "visit-java.hits"]:
        rt.require((output / name).read_bytes() == reference, "Ordered ID/f32 bits mismatch: " + name)
    rt.require((output / "visit-rust.visited").read_bytes() == (output / "visit-java.visited").read_bytes(), "Visited counts differ")
    gt = (data / "groundtruth.raw").read_bytes()
    recalls = []
    position = 0
    for qid in range(NQ):
        count = struct.unpack_from(">I", reference, position)[0]
        position += 4
        rt.require(count == K, "Unexpected result count")
        ids = [struct.unpack_from(">i", reference, position + i * 8)[0] for i in range(count)]
        position += count * 8
        rt.require(len(set(ids)) == K and all(0 <= doc < 1000000 for doc in ids), "Invalid IDs")
        recalls.append(len(set(ids) & set(struct.unpack_from("<100I", gt, qid * K * 4))) / K)
    rt.require(position == len(reference), "Trailing hit data")
    visited = struct.unpack(">10000Q", (output / "visit-rust.visited").read_bytes())
    result = {"ordered_ids_and_score_bits_equal": True, "visited_equal": True, "recall_at_100": statistics.mean(recalls),
              "recall_by_query": recalls, "visited_by_query": visited, "visited_mean": statistics.mean(visited),
              "visited_min": min(visited), "visited_max": max(visited), "visited_total": sum(visited)}
    rt.save(output / "QUALITY.json", result)
    return {key: value for key, value in result.items() if not key.endswith("_by_query")}


def verify(args, output):
    output.mkdir()
    servers, commands = launch(args, output, "verify")
    receipt = {"ok": False, "commands": commands, "requests_per_engine": 2 * NQ}
    try:
        for engine in rt.ENGINES:
            for action, tag in [("CHECK", "check-" + engine), ("VISIT", "visit-" + engine)]:
                response, _ = servers[engine].ask(action + " " + tag)
                rt.require(response.split("\t")[:2] == [action, tag], "Vector verification protocol error")
                print("VERIFIED", engine, action, flush=True)
        receipt["quality"] = quality(output, args.data)
        for engine in list(servers):
            servers[engine].close("QUIT")
            del servers[engine]
        receipt["ok"] = True
    except BaseException as error:
        receipt["error"] = repr(error)
        raise
    finally:
        for server in servers.values():
            server.abort()
        rt.save(output / "RECEIPT.json", receipt)
    return receipt


def quantile(values, probability):
    values = sorted(values)
    x = (len(values) - 1) * probability
    low = int(x)
    high = min(low + 1, len(values) - 1)
    return values[low] + (values[high] - values[low]) * (x - low)


def summarize(path):
    run = rt.load(path / "RUN.json")
    preset = run["config"]
    query_count, rounds = preset["queries"], preset["rounds"]
    report = {"schema_version": 1, "method_id": "vector-search", "protocol_version": run["protocol_version"],
              "run_receipt_sha256": rt.sha(path / "RUN.json"),
              "complete": False, "status": run["status"],
              "standard_full_run": run.get("standard_full_run", False),
              "quality": run.get("verification", {}).get("quality"), "batches": [],
              "formal_single_engine_requests": sum(len(batch.get("runs", [])) * query_count for batch in run.get("batches", []))}
    per_query = []
    for batch in run.get("batches", []):
        if len(batch["rounds"]) != rounds or not batch.get("ok"):
            continue
        totals, timings = {}, {}
        for engine in rt.ENGINES:
            rows = [row["engines"][engine] for row in batch["rounds"]]
            ns = []
            for row in rows:
                ns.extend(struct.unpack(">%dQ" % query_count, (path / (row["tag"] + ".ns")).read_bytes()))
            timings[engine] = ns
            elapsed, cpu = sum(x["elapsed_ns"] for x in rows), sum(x["cpu_ns"] for x in rows)
            totals[engine] = {"mean_ms": elapsed / len(ns) / 1e6, "serial_qps": len(ns) * 1e9 / elapsed,
                              "p50_ms": quantile(ns, .5) / 1e6, "p95_ms": quantile(ns, .95) / 1e6,
                              "p99_ms": quantile(ns, .99) / 1e6, "cpu_ms_per_query": cpu / len(ns) / 1e6,
                              "average_cpu_cores": cpu / elapsed,
                              "process_peak_rss_MiB": batch["usage"][engine]["peak_rss_bytes"] / 1048576}
        for qid in range(query_count):
            values = {e: statistics.median(timings[e][qid::query_count]) for e in rt.ENGINES}
            per_query.append({"batch": batch["batch"], "qid": qid, "rust_ms": values["rust"] / 1e6,
                              "java_ms": values["java"] / 1e6, "change_pct": 100 * (values["rust"] / values["java"] - 1)})
        changes = [100 * (x["engines"]["rust"]["elapsed_ns"] / x["engines"]["java"]["elapsed_ns"] - 1) for x in batch["rounds"]]
        report["batches"].append({"batch": batch["batch"], "engines": totals, "paired_sweep_change_pct": statistics.median(changes),
                                  "faster_sweeps": sum(x < 0 for x in changes)})
    report["complete"] = run["status"] == "COMPLETE" and len(report["batches"]) == preset["batches"]
    rt.save(path / "RESULT.json", report)
    rt.save(path / "REGRESSIONS.json", [x for x in per_query if x["change_pct"] > 0])
    with (path / "per-query.csv").open("w", newline="") as stream:
        writer = csv.DictWriter(stream, fieldnames=["batch", "qid", "rust_ms", "java_ms", "change_pct"])
        writer.writeheader()
        writer.writerows(per_query)
    lines = ["# SIFT1M Top100 query comparison", "", "Status: " + run["status"],
             "", "Adapted from luceneutil; metric definitions: benchmark/vector-search/METRICS.md.",
             "", "| Batch | Rust / Java elapsed change | Faster sweeps |", "|---|---:|---:|"]
    for batch in report["batches"]:
        lines.append("| %d | %+.3f%% | %d/%d |" % (batch["batch"], batch["paired_sweep_change_pct"], batch["faster_sweeps"], rounds))
    (path / "REPORT.md").write_text("\n".join(lines) + "\n")
    print("REPORT", path / "REPORT.md", flush=True)


def execute(args, smoke):
    path = rt.new_run(SUITE, args.output, "verify" if smoke else "full")
    run = {"status": "RUNNING", "method_id": "vector-search", "protocol_version": CONFIG["protocol_version"],
           "config": CONFIG, "machine": rt.machine(), "batches": [], "start": time.time()}
    print("OUTPUT", path, flush=True)
    try:
        with rt.Lock():
            run["build"] = rt.programs(SUITE, args.java_home)
            run["inputs"] = input_identity(args)
            run["standard_full_run"] = not smoke and run["inputs"]["index_parameter_provenance"] != "unverified-external-index"
            run["paths"] = {"index": str(args.index), "data": str(args.data)}
            before = rt.snapshot(args.index)
            run["verification"] = verify(args, path / "verify")
            if not smoke:
                for batch in [1, 2]:
                    servers, commands = launch(args, path, "batch%d" % batch)
                    rec = {"batch": batch, "commands": commands, "warmup": {}, "rounds": [], "runs": [], "usage": {}, "ok": False}
                    run["batches"].append(rec)
                    try:
                        for engine in (rt.ENGINES if batch == 1 else list(reversed(rt.ENGINES))):
                            response, _ = servers[engine].ask("WARM 60")
                            parts = response.split("\t")
                            rt.require(parts[0] == "WARM" and int(parts[1]) >= 60_000_000_000, "Incomplete vector warmup")
                            rec["warmup"][engine] = {"ns": int(parts[1]), "queries": int(parts[2]), "sweeps": int(parts[3])}
                            rt.save(path / "PROGRESS.json", run)
                            print("WARM", batch, engine, rec["warmup"][engine], flush=True)
                        for rnd in range(CONFIG["rounds"]):
                            sequence = rt.ENGINES if (batch + rnd) % 2 else list(reversed(rt.ENGINES))
                            rows = {}
                            for engine in sequence:
                                tag = "b%d-r%02d-%s" % (batch, rnd, engine)
                                response, _ = servers[engine].ask("RUN " + tag)
                                parts = response.split("\t")
                                rt.require(parts[:2] == ["RUN", tag], "Vector RUN protocol mismatch")
                                row = {"tag": tag, "elapsed_ns": int(parts[2]), "checksum": int(parts[3]), "cpu_ns": int(parts[4])}
                                rows[engine] = row
                                rec["runs"].append({"engine": engine, **row})
                                rt.save(path / "PROGRESS.json", run)
                            rt.require(rows["rust"]["checksum"] == rows["java"]["checksum"], "Result checksum mismatch")
                            rec["rounds"].append({"round": rnd, "order": sequence, "engines": rows})
                            rt.save(path / "PROGRESS.json", run)
                            print("ROUND", batch, rnd, rows, flush=True)
                        for engine in list(servers):
                            rec["usage"][engine] = servers[engine].close("QUIT")
                            del servers[engine]
                        rec["ok"] = True
                    finally:
                        for server in servers.values():
                            server.abort()
                        rt.save(path / "PROGRESS.json", run)
            rt.require(rt.programs(SUITE, args.java_home) == run["build"], "Programs changed during run")
            rt.require(rt.snapshot(args.index) == before, "Index changed during run")
            rt.require(input_identity(args) == run["inputs"], "Vector input changed during run")
            run["status"] = "SMOKE_OK" if smoke else "COMPLETE"
    except BaseException as error:
        run.update(status="STOPPED_BY_USER" if isinstance(error, KeyboardInterrupt) else "FAILED", error=repr(error), traceback=traceback.format_exc())
        raise
    finally:
        run["end"] = time.time()
        rt.save(path / "RUN.json", run)
        if not smoke:
            summarize(path)
    print(run["status"], path, flush=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("action", choices=["deps", "build", "prepare-index", "check", "smoke", "run", "summarize"])
    parser.add_argument("--index", type=Path, default=SUITE.parent / "data/sift1m/index")
    parser.add_argument("--data", type=Path, default=SUITE.parent / "data/sift1m")
    parser.add_argument("--java-home")
    parser.add_argument("--jars-dir", type=Path)
    parser.add_argument("--output", type=Path)
    args = parser.parse_args()
    rt.macos()
    args.index, args.data = args.index.expanduser().resolve(), args.data.expanduser().resolve()
    if args.action == "deps":
        rt.dependencies(args.jars_dir)
    elif args.action == "build":
        rt.build(SUITE, "sift-lucene-metrics", "SiftBench", args.java_home, args.jars_dir)
    elif args.action == "summarize":
        rt.require(args.output is not None, "summarize requires --output")
        summarize(args.output.resolve())
    elif args.action == "check":
        print(json.dumps(input_identity(args), indent=2))
    elif args.action == "prepare-index":
        expected = rt.load(SUITE.parent / "datasets/sift1m/manifest.json")["files"]["base.raw"]
        rt.require(rt.sha(args.data / "base.raw") == expected["sha256"], "Wrong SIFT base vectors")
        rt.require(not args.index.exists(), "Index output already exists; choose a new directory")
        with rt.Lock():
            command = rt.commands(SUITE, {"rust": [], "java": ["build", args.index, args.data / "base.raw"]}, args.java_home, heap=("512m", "6g"), gc=False)["java"]
            args.index.parent.mkdir(parents=True, exist_ok=True)
            subprocess.run(command, check=True, env=rt.environment())
            rt.save(args.index.parent / (args.index.name + "-manifest.json"),
                    {"method_id": "vector-search", "kind": "setup-only", "index_config": INDEX_CONFIG,
                     "command": command, "base_raw_sha256": expected["sha256"],
                     "java_jars": rt.programs(SUITE, args.java_home)["jars"],
                     "index_files": rt.files(args.index, exclude=("write.lock",))})
    else:
        execute(args, args.action == "smoke")


if __name__ == "__main__":
    if len(sys.argv) > 1 and sys.argv[1].startswith("release-"):
        import os
        os.execv(sys.executable, [sys.executable, "-B", str(SUITE / "release_suite.py"), *sys.argv[1:]])
    # Preserve the original SIFT query CLI; new presets use explicit stages.
    if any(arg == "--preset" or arg.startswith("--preset=") for arg in sys.argv) or (len(sys.argv) > 1 and sys.argv[1] in {
        "presets", "plan", "prepare-data", "truth", "build-index", "verify-index", "query", "all", "report"
    }):
        import os
        os.execv(sys.executable, [sys.executable, "-B", str(SUITE / "reproduce.py"), *sys.argv[1:]])
    try:
        main()
    except KeyboardInterrupt:
        sys.exit(130)
