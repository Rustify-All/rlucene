"""Build the standalone multi-dataset drivers; never modify the checkout."""
import difflib
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import time

from common import runtime as rt

SUITE = Path(__file__).resolve().parents[1]
DRIVERS = Path(__file__).resolve().parent
DESTINATION = rt.CACHE / "vector-search-reproduction"
BINARIES = {"query": "sift-lucene-metrics", "writer": "writer_metrics", "verify": "verify_index"}
CODEC_BUILDER = '''
  /// Configure the vectors format in this benchmark's frozen source copy.
  #[must_use]
  pub fn with_knn_vectors_format(mut self, format: KnnVectorsFormats) -> Self {
    self.knn_vectors_format = PerFieldKnnVectorsFormat::new(Lucene101CodecKnnVectorsFormatBase {
      default_knn_vectors_format: format,
    });
    self
  }
'''


def inputs():
    return rt.suite_inputs(SUITE)


def build(java_home=None, jars_dir=None, lock_file=None):
    """Caller holds the common exclusive lock. Builds are explicit setup only."""
    rt.macos()
    DESTINATION.mkdir(parents=True, exist_ok=True)
    attempt = DESTINATION / ("build-" + time.strftime("%Y%m%d-%H%M%S"))
    attempt.mkdir()
    receipt = {"ok": False, "start": time.time(), "source": rt.source_identity(),
               "suite_inputs": inputs(), "ordinary_release": True, "commands": []}
    try:
        home = rt.jdk(java_home)
        jars = rt.dependencies(jars_dir)
        source, consumer = DESTINATION / "source", DESTINATION / "consumer"
        for path in (source, consumer, DESTINATION / "classes"):
            if path.exists():
                path.rename(attempt / (path.name + "-previous"))
        source.mkdir()
        # Current bytes, including review edits, are copied with fresh mtimes.
        # This prevents Cargo Fresh from reusing an older source snapshot.
        tracked = set(rt.output(["git", "-C", rt.GIT_REPOSITORY, "ls-tree", "-r", "--name-only",
                                 rt.SOURCE_COMMIT]).splitlines()) if rt.SOURCE_COMMIT else set(
                                     rt.output(["git", "-C", rt.REPOSITORY, "ls-files"]).splitlines())
        tracked.update(receipt["source"]["files"])
        for relative in sorted(tracked):
            path = rt.REPOSITORY / relative
            if relative.startswith("benchmark/") or not path.is_file():
                continue
            target = source / relative
            target.parent.mkdir(parents=True, exist_ok=True)
            shutil.copyfile(path, target)
        codec = source / "src/core/codecs/lucene101_codec.rs"
        original = codec.read_text(encoding="utf-8")
        if "pub fn with_knn_vectors_format(" not in original:
            marker = "impl Lucene101Codec {"
            rt.require(original.count(marker) == 1, "Codec layout changed; inspect before adapting")
            codec.write_text(original.replace(marker, marker + CODEC_BUILDER), encoding="utf-8")
        patch = "".join(difflib.unified_diff(original.splitlines(True), codec.read_text().splitlines(True),
                                           fromfile="a/src/core/codecs/lucene101_codec.rs",
                                           tofile="b/src/core/codecs/lucene101_codec.rs"))
        (attempt / "codec-configuration.diff").write_text(patch, encoding="utf-8")
        shutil.copytree(DRIVERS / "rust", consumer)
        if lock_file is not None:
            shutil.copyfile(lock_file, consumer / "Cargo.lock")
            receipt["selected_cargo_lock"] = {"path": str(lock_file), "sha256": rt.sha(lock_file)}
        manifest = consumer / "Cargo.toml"
        text = manifest.read_text().replace('path = "../../../.."', 'path = "../source"')
        manifest.write_text(text, encoding="utf-8")
        classes = DESTINATION / "classes"
        classes.mkdir()
        commands = [
            ["cargo", "+1.98.1", "build", "--release", "--locked", "-vv", "--manifest-path",
             consumer / "Cargo.toml", "--target-dir", DESTINATION / "target"],
            [Path(home["home"]) / "bin/javac", "--release", "23", "--add-modules", "jdk.incubator.vector",
             "-encoding", "UTF-8", "-cp", os.pathsep.join(str(p) for p in sorted(jars.glob("*.jar"))),
             "-d", classes, *sorted((DRIVERS / "java").glob("*.java")),
             *sorted((rt.BENCHMARK / "datasets/vector-wiki/upstream").rglob("*.java"))]]
        receipt.update(jdk=home, rustc=rt.output(["rustc", "+1.98.1", "-Vv"]),
                       cargo=rt.output(["cargo", "+1.98.1", "--version"]))
        for name, command in zip(("rust", "java"), commands):
            command = list(map(str, command))
            receipt["commands"].append(command)
            print("COMPILE", name, attempt, flush=True)
            with (attempt / (name + ".log")).open("wb") as log:
                child = subprocess.Popen(command, cwd=rt.REPOSITORY, env=rt.environment(),
                                         stdout=log, stderr=subprocess.STDOUT)
                try:
                    code = child.wait()
                except BaseException:
                    child.terminate()
                    child.wait()
                    raise
            rt.require(code == 0, "Compile failed; retained log: " + str(attempt / (name + ".log")))
        lines = (attempt / "rust.log").read_text().splitlines()
        actual = [line for line in lines if "Running " in line and "--crate-name rlucene " in line]
        rt.require(len(actual) == 1, "Missing unique library rustc command; inspect retained build log")
        metadata = re.search(r"-C metadata=([0-9a-f]+)", actual[0])
        rt.require(metadata is not None and "-C opt-level=3" in actual[0], "Unexpected release metadata")
        rt.require(not any(flag in actual[0] for flag in ("target-cpu=native", "-C lto", "profile-use", "profile-generate")),
                   "Nonstandard compiler flags")
        rt.require(rt.source_identity() == receipt["source"] and inputs() == receipt["suite_inputs"],
                   "Source changed while compiling; receipt retained, build not accepted")
        receipt.update(ok=True, actual_rlucene_metadata=metadata[1], actual_rustc=actual[0],
                       codec_patch_sha256=rt.sha(attempt / "codec-configuration.diff"),
                       library_modified=bool(patch), source_path=str(source), consumer_path=str(consumer),
                       target_path=str(DESTINATION / "target"), source_files=rt.files(source),
                       consumer_files=rt.files(consumer), classes=rt.files(classes), jars=rt.files(jars),
                       binaries={role: rt.sha(DESTINATION / "target/release" / name)
                                 for role, name in BINARIES.items()}, build_evidence=str(attempt))
    except BaseException as error:
        receipt["error"] = repr(error)
        raise
    finally:
        receipt["end"] = time.time()
        rt.save(attempt / "BUILD.json", receipt)
        if receipt["ok"]:
            rt.save(DESTINATION / "BUILD.json", receipt)


def programs(java_home=None):
    receipt = rt.load(DESTINATION / "BUILD.json")
    rt.require(receipt["ok"] and receipt["source"] == rt.source_identity(), "Build missing or library changed")
    rt.require(receipt["suite_inputs"] == inputs(), "Benchmark changed; explicitly rebuild the drivers")
    for key, path in (("source_files", "source"), ("consumer_files", "consumer"), ("classes", "classes")):
        rt.require(rt.files(DESTINATION / path) == receipt[key], "Frozen build changed: " + path)
    rt.require(rt.files(rt.CACHE / "java-jars") == receipt["jars"], "Java JAR identities changed")
    rt.require(rt.jdk(java_home) == receipt["jdk"], "JDK changed after compilation")
    for role, name in BINARIES.items():
        rt.require(rt.sha(DESTINATION / "target/release" / name) == receipt["binaries"][role], "Binary changed")
    return receipt


def command(receipt, engine, role, args):
    if engine == "rust":
        return [str(DESTINATION / "target/release" / BINARIES[role]), *map(str, args)]
    cp = os.pathsep.join(map(str, [DESTINATION / "classes", *sorted((rt.CACHE / "java-jars").glob("*.jar"))]))
    return [str(Path(receipt["jdk"]["home"]) / "bin/java"), "--add-modules", "jdk.incubator.vector",
            "--enable-native-access=ALL-UNNAMED", "-Xms512m", "-Xmx6g", "-cp", cp,
            {"query": "SiftBench", "writer": "WriterMetrics", "verify": "VerifyIndex",
             "truth": "ExactGroundTruth", "generate": "WikiVectors"}[role], *map(str, args)]
