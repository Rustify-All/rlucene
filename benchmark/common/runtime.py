"""Small shared helpers for the officially supported macOS benchmark runners."""
import fcntl
import hashlib
import json
import os
from pathlib import Path
import platform
import select
import shutil
import subprocess
import sys
import time
import urllib.request

BENCHMARK = Path(__file__).resolve().parents[1]
REPOSITORY = BENCHMARK.parent
CACHE = BENCHMARK / ".cache"
ENGINES = ["rust", "java"]
GIT_REPOSITORY = REPOSITORY
SOURCE_COMMIT = None
LOCK_FD = None
if os.environ.get("RLUCENE_BENCHMARK_CONTEXT"):
    context = json.loads(Path(os.environ["RLUCENE_BENCHMARK_CONTEXT"]).read_text())
    REPOSITORY = Path(context["source"])
    GIT_REPOSITORY = Path(context["git_repository"])
    CACHE = Path(context["cache"])
    SOURCE_COMMIT = context["commit"]
    LOCK_FD = int(context["lock_fd"])


def require(condition, message):
    if not condition:
        raise RuntimeError(message)


def load(path):
    return json.loads(Path(path).read_text(encoding="utf-8"))


def save(path, value):
    Path(path).write_text(json.dumps(value, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")


def sha(path):
    digest = hashlib.sha256()
    with Path(path).open("rb") as stream:
        for block in iter(lambda: stream.read(8 * 1024 * 1024), b""):
            digest.update(block)
    return digest.hexdigest()


def files(directory, exclude=()):
    return {p.relative_to(directory).as_posix(): {"bytes": p.stat().st_size, "sha256": sha(p)}
            for p in sorted(directory.rglob("*")) if p.is_file() and p.name not in exclude}


def snapshot(directory):
    return {p.name: [p.stat().st_size, p.stat().st_mtime_ns]
            for p in sorted(directory.iterdir()) if p.is_file() and p.name != "write.lock"}


def macos():
    require(sys.platform == "darwin", "Official runner support is macOS. See README for porting notes.")


class Lock:
    def __enter__(self):
        if LOCK_FD is not None:
            held = os.fstat(LOCK_FD)
            target = os.stat("/tmp/rlucene-performance-build.lock")
            require((held.st_dev, held.st_ino) == (target.st_dev, target.st_ino),
                    "Inherited descriptor is not the shared performance lock")
            self.stream = os.fdopen(os.dup(LOCK_FD), "a")
            try:
                fcntl.flock(self.stream.fileno(), fcntl.LOCK_EX | fcntl.LOCK_NB)
            except BaseException:
                self.stream.close()
                raise
            return self
        self.stream = open("/tmp/rlucene-performance-build.lock", "a")
        print("WAIT /tmp/rlucene-performance-build.lock", flush=True)
        try:
            fcntl.flock(self.stream, fcntl.LOCK_EX)
        except BaseException:
            self.stream.close()
            raise
        return self

    def __exit__(self, *_):
        self.stream.close()


def environment():
    env = os.environ.copy()
    for key in list(env):
        if key in {"JAVA_TOOL_OPTIONS", "_JAVA_OPTIONS", "JDK_JAVA_OPTIONS", "RUSTFLAGS",
                   "CARGO_ENCODED_RUSTFLAGS", "RUSTC_WRAPPER", "RUSTC_WORKSPACE_WRAPPER",
                   "RUSTC", "CARGO_BUILD_TARGET", "CARGO_TARGET_DIR", "CARGO_BUILD_RUSTFLAGS"} or (
                key.startswith("CARGO_PROFILE_") or key.startswith("CARGO_TARGET_") and key.endswith("_RUSTFLAGS")):
            del env[key]
    return env


def output(command):
    done = subprocess.run(list(map(str, command)), capture_output=True, text=True, check=True, env=environment())
    return (done.stdout + done.stderr).strip()


def jdk(selected=None):
    home = selected or os.environ.get("JAVA_HOME")
    if not home:
        home = output(["/usr/libexec/java_home", "-v", "23"])
    home = Path(home).expanduser().resolve()
    require((home / "bin/javac").is_file(), "A complete JDK 23 is required; set JAVA_HOME.")
    version = output([home / "bin/java", "-version"])
    require('version "23' in version, "Expected JDK 23; found " + version)
    return {"home": str(home), "version": version, "java_sha256": sha(home / "bin/java"),
            "javac_sha256": sha(home / "bin/javac"), "jvm_sha256": sha(home / "lib/server/libjvm.dylib")}


def source_identity(repository=None, commit=None):
    repository = Path(repository or REPOSITORY)
    commit = commit or SOURCE_COMMIT
    paths = [repository / "Cargo.toml", repository / "rust-toolchain.toml"]
    paths += sorted((repository / "src").rglob("*.rs"))
    hashes = {p.relative_to(repository).as_posix(): sha(p) for p in paths}
    if commit:
        entries = subprocess.check_output(["git", "-C", str(GIT_REPOSITORY), "ls-tree", "-rz", commit]).split(b"\0")
        expected = {}
        for entry in filter(None, entries):
            meta, filename = entry.split(b"\t", 1)
            name = os.fsdecode(filename)
            if name in ("Cargo.toml", "rust-toolchain.toml") or name.startswith("src/") and name.endswith(".rs"):
                expected[name] = meta.split()[2].decode()
        modified = set(expected).symmetric_difference(hashes)
        for name in expected.keys() & hashes.keys():
            data = (repository / name).read_bytes()
            if hashlib.sha1(f"blob {len(data)}\0".encode() + data).hexdigest() != expected[name]:
                modified.add(name)
        modified = sorted(modified)
    else:
        commit = output(["git", "-C", repository, "rev-parse", "HEAD"])
        modified = output(["git", "-C", repository, "diff", "HEAD", "--name-only", "--", "src", "Cargo.toml", "rust-toolchain.toml"]).splitlines()
    return {"commit": commit,
            "source_sha256": hashlib.sha256(json.dumps(hashes, sort_keys=True).encode()).hexdigest(),
            "files": hashes,
            "modified_library_files": modified}


def download(url, path, expected):
    path = Path(path)
    path.parent.mkdir(parents=True, exist_ok=True)
    if path.exists():
        require(sha(path) == expected, "Existing file has wrong SHA256: " + str(path))
        return
    partial = path.with_name(path.name + ".partial")
    require(not partial.exists(), "Partial download exists; inspect it before retrying: " + str(partial))
    print("DOWNLOAD", url, flush=True)
    request = urllib.request.Request(url, headers={"User-Agent": "RLucene-benchmark/1"})
    with urllib.request.urlopen(request, timeout=120) as response, partial.open("xb") as stream:
        shutil.copyfileobj(response, stream, 8 * 1024 * 1024)
    require(sha(partial) == expected, "Downloaded SHA256 mismatch; partial file retained: " + str(partial))
    partial.rename(path)


def dependencies(local=None):
    destination = CACHE / "java-jars"
    destination.mkdir(parents=True, exist_ok=True)
    for item in load(BENCHMARK / "common/java-jars.json"):
        path = destination / item["file"]
        if local and not path.exists():
            original = Path(local) / item["file"]
            require(sha(original) == item["sha256"], "Local JAR mismatch: " + str(original))
            shutil.copyfile(original, path)
        else:
            download(item["url"], path, item["sha256"])
        require(sha(path) == item["sha256"], "JAR identity changed: " + str(path))
    return destination


def suite_inputs(suite):
    paths = [*suite.rglob("*"), * (BENCHMARK / "common").rglob("*"),
             *(BENCHMARK / "datasets").rglob("*"), BENCHMARK / "run.py"]
    return {p.relative_to(BENCHMARK).as_posix(): sha(p)
            for p in sorted(paths) if p.is_file() and not {"__pycache__", "target"}.intersection(p.parts)
            and p.suffix not in {".pyc", ".class"}}


def build(suite, binary, main_class, java_home=None, local_jars=None):
    macos()
    with Lock():
        jars = dependencies(local_jars)
        home = jdk(java_home)
        destination = CACHE / suite.name
        destination.mkdir(parents=True, exist_ok=True)
        attempt = destination / time.strftime("build-%Y%m%d-%H%M%S")
        attempt.mkdir()
        classes = destination / "classes"
        classes.mkdir(exist_ok=True)
        receipt = {"ok": False, "source": source_identity(), "suite_inputs": suite_inputs(suite),
                   "jdk": home, "start": time.time(), "path": str(destination)}
        commands = [
            ["cargo", "+1.98.1", "build", "--locked", "--release", "--manifest-path", suite / "rust/Cargo.toml",
             "--target-dir", destination / "target", "-v"],
            [Path(home["home"]) / "bin/javac", "--release", "23", "-encoding", "UTF-8", "-cp",
             os.pathsep.join(str(p) for p in sorted(jars.glob("*.jar"))), "-d", classes,
             *sorted((suite / "java").glob("*.java"))]]
        receipt["commands"] = [list(map(str, command)) for command in commands]
        try:
            receipt["rustc"] = output(["rustc", "+1.98.1", "-Vv"])
            receipt["cargo"] = output(["cargo", "+1.98.1", "--version"])
            for name, command in zip(["rust", "java"], commands):
                print("BUILD", name, attempt, flush=True)
                with (attempt / (name + ".log")).open("wb") as log:
                    child = subprocess.Popen(list(map(str, command)), cwd=REPOSITORY, env=environment(), stdout=log, stderr=subprocess.STDOUT)
                    try:
                        code = child.wait()
                    except BaseException:
                        child.terminate()
                        child.wait()
                        raise
                require(code == 0, "Build failed; see " + str(attempt / (name + ".log")))
            require(source_identity() == receipt["source"], "Library source changed while building")
            require(suite_inputs(suite) == receipt["suite_inputs"], "Suite changed while building")
            receipt.update(ok=True, binary=binary, main_class=main_class,
                           binary_sha256=sha(destination / "target/release" / binary), classes=files(classes), jars=files(jars))
        except BaseException as error:
            receipt["error"] = repr(error)
            raise
        finally:
            receipt["end"] = time.time()
            save(attempt / "BUILD.json", receipt)
            if receipt["ok"]:
                save(destination / "BUILD.json", receipt)
    print("BUILT", destination, flush=True)


def programs(suite, java_home=None):
    destination = CACHE / suite.name
    receipt = load(destination / "BUILD.json")
    require(receipt["ok"], "Run build first")
    require(source_identity() == receipt["source"], "Library changed after build; rebuild this suite")
    require(suite_inputs(suite) == receipt["suite_inputs"], "Suite changed after build; rebuild this suite")
    require(sha(destination / "target/release" / receipt["binary"]) == receipt["binary_sha256"], "Binary changed")
    require(files(destination / "classes") == receipt["classes"], "Java classes changed")
    require(files(CACHE / "java-jars") == receipt["jars"], "Java JARs changed")
    require(jdk(java_home) == receipt["jdk"], "JDK changed after build")
    return receipt


def commands(suite, arguments, java_home=None, heap=("4g", "4g"), gc=True):
    receipt = programs(suite, java_home)
    destination = CACHE / suite.name
    cp = os.pathsep.join(map(str, [destination / "classes", *sorted((CACHE / "java-jars").glob("*.jar"))]))
    java = [str(Path(receipt["jdk"]["home"]) / "bin/java"), "--add-modules", "jdk.incubator.vector",
            "--enable-native-access=ALL-UNNAMED", "-Xms" + heap[0], "-Xmx" + heap[1]]
    if gc:
        java.append("-XX:+UseParallelGC")
    java += ["-cp", cp, receipt["main_class"]]
    return {"rust": [str(destination / "target/release" / receipt["binary"]), *map(str, arguments["rust"])],
            "java": [*java, *map(str, arguments["java"])]}


class Server:
    """The original macOS select/read pipe backend; no platform abstraction."""
    def __init__(self, command, log, timeout=120):
        self.command, self.timeout, self.buffer = command, timeout, b""
        self.log = Path(log).open("wb")
        try:
            self.process = subprocess.Popen(command, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                            stderr=self.log, bufsize=0, env=environment(), start_new_session=True)
        except BaseException:
            self.log.close()
            raise

    def read(self):
        deadline = time.monotonic() + self.timeout
        while b"\n" not in self.buffer:
            remaining = deadline - time.monotonic()
            require(remaining > 0, "Engine response timed out: " + str(self.command))
            ready, _, _ = select.select([self.process.stdout], [], [], remaining)
            require(ready, "Engine response timed out: " + str(self.command))
            block = os.read(self.process.stdout.fileno(), 65536)
            require(block, "Engine exited before response: " + str(self.command))
            self.buffer += block
        line, self.buffer = self.buffer.split(b"\n", 1)
        return line.decode("utf-8").rstrip("\r")

    def ask(self, request):
        start = time.perf_counter_ns()
        self.process.stdin.write((request + "\n").encode("utf-8"))
        self.process.stdin.flush()
        response = self.read()
        return response, time.perf_counter_ns() - start

    def close(self, quit_command=None):
        if quit_command:
            self.process.stdin.write((quit_command + "\n").encode())
            self.process.stdin.flush()
        self.process.stdin.close()
        deadline = time.monotonic() + 60
        while True:
            pid, status, usage = os.wait4(self.process.pid, os.WNOHANG)
            if pid:
                self.process.returncode = os.waitstatus_to_exitcode(status)
                break
            require(time.monotonic() < deadline, "Engine did not exit")
            time.sleep(0.01)
        self.process.stdout.close()
        self.log.close()
        require(self.process.returncode == 0, "Engine failed with exit code " + str(self.process.returncode))
        return {"peak_rss_bytes": usage.ru_maxrss, "user_cpu_seconds": usage.ru_utime, "system_cpu_seconds": usage.ru_stime}

    def abort(self):
        if self.process.returncode is None:
            self.process.terminate()
            try:
                self.process.wait(timeout=5)
            except subprocess.TimeoutExpired:
                self.process.kill()
                self.process.wait()
        self.process.stdin.close()
        self.process.stdout.close()
        self.log.close()


def new_run(suite, selected=None, prefix="run"):
    path = Path(selected).expanduser().resolve() if selected else BENCHMARK / "runs" / suite.name / (prefix + "-" + time.strftime("%Y%m%d-%H%M%S"))
    require(not path.exists(), "Output directory already exists: " + str(path))
    path.mkdir(parents=True)
    return path


def machine():
    return {"os": platform.platform(), "architecture": platform.machine(), "python": sys.version,
            "repository_path": str(REPOSITORY), "cpu_count": os.cpu_count()}
