"""Download and reproduce the pinned benchmark inputs; Python standard library only."""
import argparse
import bz2
from pathlib import Path
import shutil
import struct
import subprocess
import sys
import tarfile

BENCHMARK = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(BENCHMARK))
from common import runtime as rt
from ms_marco_passage import prepare as ms_marco_passage
from rally_http_logs import prepare as rally_http_logs


def finish(partial, destination, expected):
    rt.require(partial.stat().st_size == expected["bytes"] and rt.sha(partial) == expected["sha256"],
               "Input identity mismatch; partial retained: " + str(partial))
    partial.rename(destination)


def ready(destination, expected):
    if not destination.exists():
        rt.require(not destination.with_suffix(destination.suffix + ".partial").exists(), "Partial exists; inspect before retry: " + str(destination))
        return False
    rt.require(destination.stat().st_size == expected["bytes"] and rt.sha(destination) == expected["sha256"],
               "Existing input differs: " + str(destination))
    return True


def wiki(args):
    meta = rt.load(BENCHMARK / "datasets/wiki/manifest.json")
    archive = args.archive or args.destination / "wiki-articles.json.bz2"
    if args.archive:
        rt.require(rt.sha(archive) == meta["corpus"]["compressed_sha256"], "Wrong Wiki archive")
    else:
        rt.download(meta["download_url"], archive, meta["corpus"]["compressed_sha256"])
    target = args.destination / "corpus.json"
    if ready(target, meta["corpus"]):
        print("READY", target)
        return
    transform = BENCHMARK / "datasets/wiki/corpus_transform.py"
    rt.require(rt.sha(transform) == meta["transform_sha256"], "Upstream transformation changed")
    partial = target.with_suffix(".json.partial")
    env = rt.environment()
    env["PYTHONIOENCODING"] = "utf-8"
    with bz2.open(archive, "rb") as source, partial.open("xb") as output:
        process = subprocess.Popen([sys.executable, "-B", str(transform)], stdin=subprocess.PIPE, stdout=output, env=env)
        try:
            shutil.copyfileobj(source, process.stdin, 8 * 1024 * 1024)
            process.stdin.close()
            rt.require(process.wait() == 0, "Wiki transform failed")
        except BaseException:
            process.terminate()
            process.wait()
            raise
    finish(partial, target, meta["corpus"])
    print("READY", target, flush=True)


def sift(args):
    meta = rt.load(BENCHMARK / "datasets/sift1m/manifest.json")
    archive = args.archive or args.destination / "sift.tar.gz"
    if args.archive:
        rt.require(rt.sha(archive) == meta["archive_sha256"], "Wrong SIFT archive")
    else:
        rt.download(meta["mirror"], archive, meta["archive_sha256"])
    specs = [("sift_base.fvecs", "base.raw", 1000000, 128),
             ("sift_query.fvecs", "query.raw", 10000, 128),
             ("sift_groundtruth.ivecs", "groundtruth.raw", 10000, 100)]
    with tarfile.open(archive, "r:gz") as tar:
        members = tar.getmembers()
        for name, _, _, _ in specs:
            target = args.destination / name
            if ready(target, meta["files"][name]):
                continue
            matches = [member for member in members if member.isfile() and Path(member.name).name == name]
            rt.require(len(matches) == 1, "Ambiguous or missing SIFT archive member: " + name)
            partial = target.with_suffix(target.suffix + ".partial")
            with tar.extractfile(matches[0]) as source, partial.open("xb") as output:
                shutil.copyfileobj(source, output, 8 * 1024 * 1024)
            finish(partial, target, meta["files"][name])
    for name, output_name, count, dimensions in specs:
        target = args.destination / output_name
        if ready(target, meta["files"][output_name]):
            continue
        partial = target.with_suffix(".raw.partial")
        with (args.destination / name).open("rb") as source, partial.open("xb") as output:
            for _ in range(count):
                header = source.read(4)
                rt.require(len(header) == 4 and struct.unpack("<I", header)[0] == dimensions, "SIFT dimension mismatch")
                payload = source.read(dimensions * 4)
                rt.require(len(payload) == dimensions * 4, "Truncated SIFT input")
                output.write(payload)
            rt.require(source.read(1) == b"", "Trailing SIFT input")
        finish(partial, target, meta["files"][output_name])
        print("READY", target, flush=True)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    datasets = {"wiki": wiki, "sift1m": sift, "rally-http-logs-1m": rally_http_logs,
                "ms-marco-passage-1m": ms_marco_passage}
    parser.add_argument("dataset", choices=datasets)
    parser.add_argument("--destination", type=Path)
    parser.add_argument("--archive", type=Path, help="Reuse an existing exact input archive")
    parser.add_argument("--queries-archive", type=Path, help="Reuse the exact MS MARCO queries.tar.gz")
    args = parser.parse_args()
    if args.queries_archive and args.dataset != "ms-marco-passage-1m":
        parser.error("--queries-archive is only for ms-marco-passage-1m")
    rt.macos()
    args.destination = (args.destination or BENCHMARK / "data" / args.dataset).expanduser().resolve()
    args.destination.mkdir(parents=True, exist_ok=True)
    if args.archive:
        args.archive = args.archive.expanduser().resolve()
    if args.queries_archive:
        args.queries_archive = args.queries_archive.expanduser().resolve()
    with rt.Lock():
        datasets[args.dataset](args)
