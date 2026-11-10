"""Shared process protocol, sample order and paired statistics for indexing."""
from contextlib import ExitStack, contextmanager
from itertools import permutations
import json
import subprocess
from statistics import median

from common import runtime as rt

ENGINES = ("java", "rust", "tuned")
METRICS = ("write_ns", "commit_ns", "close_ns", "total_ns")
ORDERS = ((0, 5, 1, 3, 2, 4), (4, 2, 3, 1, 5, 0), (2, 1, 5, 0, 4, 3))


def append(path, value):
    with path.open("a", encoding="utf-8") as stream:
        stream.write(json.dumps(value, ensure_ascii=False) + "\n")


def read_rows(path):
    return [json.loads(line) for line in path.read_text().splitlines()]


def checked(command, log, env=None, cwd=rt.REPOSITORY):
    with log.open("wb") as stream, log.with_suffix(log.suffix + ".err").open("wb") as errors:
        subprocess.run(list(map(str, command)), cwd=cwd, env=env or rt.environment(),
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


def write(ask, index, preset):
    result = ask(index)
    rt.require(result["docs"] == preset["documents"]
               and all(result[key] > 0 for key in METRICS)
               and result["total_ns"] == sum(result[k] for k in METRICS[:-1]),
               "Writer result mismatch: " + str(result))
    return result


def measure(commands, data, preset, out, verify):
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
            verify(label, index)


def summarize(out):
    receipt = rt.load(out / "RUN.json")
    rt.require(receipt["status"] == "complete" and receipt["action"] == "run", "A complete timed run is required")
    samples = [r for r in read_rows(out / "samples.jsonl") if r["phase"] == "formal"]
    preset = receipt["preset"]
    rt.require(len(samples) == preset["batches"] * preset["formal_per_batch"], "Incomplete samples")
    engines = tuple(receipt["commands"])
    pairs_to_compare = preset.get("comparisons", (("rust", "java"), ("tuned", "java"), ("tuned", "rust")))

    def stats(block):
        result = {}
        for metric in METRICS:
            values = {e: [r["engines"][e][metric] for r in block] for e in engines}
            comparisons = {}
            for candidate, reference in pairs_to_compare:
                if candidate in values and reference in values:
                    pairs = list(zip(values[candidate], values[reference]))
                    changes = [100 * (a / b - 1) for a, b in pairs]
                    comparisons[f"{candidate}_vs_{reference}"] = {
                        "median_change_pct": median(changes), "faster_rounds": sum(a < b for a, b in pairs),
                        "rounds": len(pairs), "paired_changes_pct": changes,
                        "median_difference_ns": median([a - b for a, b in pairs]),
                        "paired_difference_ns": [a - b for a, b in pairs],
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
