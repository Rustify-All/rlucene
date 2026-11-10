"""Fetch and verify the inputs pinned by the published structured indexing reports."""
import importlib
from pathlib import Path
import shutil

from common import runtime as rt

DATASETS = {
    "geonames-1m": "geonames",
    "nyc-yellow-taxi-2025-01-1m": "nyc-yellow-taxi",
}


def verify(data, metadata):
    for name, expected in metadata["derived"].items():
        path = data / name
        rt.require(path.stat().st_size == expected["bytes"] and rt.sha(path) == expected["sha256"],
                   f"Derived input differs from the published sample: {path}")


def prepare(dataset, destination, archive=None):
    workload = DATASETS[dataset]
    metadata = rt.load(Path(__file__).resolve().parent / workload / "manifest.json")
    source = metadata["archive"]
    destination.mkdir(parents=True, exist_ok=True)
    target = destination / source["name"]
    if archive is not None:
        rt.require(rt.sha(archive) == source["sha256"], f"Archive SHA-256 mismatch: {archive}")
        if not target.exists():
            partial = target.with_suffix(target.suffix + ".partial")
            with archive.open("rb") as origin, partial.open("xb") as copy:
                shutil.copyfileobj(origin, copy, 8 * 1024 * 1024)
            rt.require(rt.sha(partial) == source["sha256"], f"Archive changed during copy: {archive}")
            partial.rename(target)
    if workload == "geonames":
        rt.require(target.is_file(), "Supply the 2026-11-10 GeoNames ZIP with --archive. The official URL changes daily.")
    rt.download(source["url"], target, source["sha256"])

    sample = destination / ("sample-" + source["sha256"][:16])
    if sample.exists():
        verify(sample, metadata)
        manifest = rt.load(sample / "MANIFEST.json")
        rt.require(manifest["snapshot"]["sha256"] == source["sha256"]
                   and manifest["files"] == metadata["derived"], f"Sample manifest differs: {sample}")
    else:
        module = importlib.import_module(workload.replace("-", "_"))
        staging = sample.with_name(sample.name + ".in-progress")
        staging.mkdir()
        manifest = module.derive(target, staging, metadata)
        verify(staging, metadata)
        manifest["files"] = metadata["derived"]
        rt.save(staging / "MANIFEST.json", manifest)
        staging.rename(sample)
    return sample, manifest, target
