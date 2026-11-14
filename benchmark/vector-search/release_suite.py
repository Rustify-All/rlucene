#!/usr/bin/env python3
"""Freeze and reproduce the six measured Alpha.3 vector configurations."""
import argparse
import io
import json
import os
from pathlib import Path
import shutil
import signal
import subprocess
import sys
import tarfile
import time

SUITE = Path(__file__).resolve().parent
sys.path.insert(0, str(SUITE.parent))
from common import runtime as rt


def save(path, value):
    temporary = path.with_suffix(path.suffix + '.tmp')
    rt.save(temporary, value)
    temporary.replace(path)


def configuration(args):
    release = rt.load(SUITE / 'release.json')
    for name, digest in release['measurement_files'].items():
        rt.require(rt.sha(rt.BENCHMARK / name) == digest, 'Measured driver changed: ' + name)
    lock = SUITE / release['cargo_lock']
    rt.require(rt.sha(lock) == release['cargo_lock_sha256'], 'Release Cargo.lock changed')
    workloads = []
    for name in release['presets']:
        path = SUITE / 'presets' / (name + '.json')
        rt.require(rt.sha(path) == release['preset_sha256'][name], 'Release preset changed: ' + name)
        workloads.append(rt.load(path))
    output = (args.output or rt.BENCHMARK / 'runs/vector-release/alpha3').expanduser().resolve()
    data = (args.data_root or rt.BENCHMARK / 'data').expanduser().resolve()
    indices = (args.index_root or rt.BENCHMARK / 'indexing/runs' / output.name).expanduser().resolve()
    stages = [{'action': 'build'}]
    for config in workloads:
        name = config['id']
        stages.append({'action': 'derive' if name == 'minilm-wiki-384' else 'prepare', 'preset': name})
        if name != 'sift1m':
            stages.append({'action': 'truth', 'preset': name})
        stages.append({'action': 'all', 'preset': name})
    stages.append({'action': 'audit'})
    return {'release': release, 'configurations': workloads, 'output': str(output),
            'data_root': str(data), 'index_root': str(indices),
            'sources': str((args.sources or data / 'vector-sources').expanduser().resolve()),
            'java_home': args.java_home, 'jars_dir': str(args.jars_dir.resolve()) if args.jars_dir else None,
            'inference_python': str(args.inference_python.resolve()) if args.inference_python else None,
            'download': args.download, 'stages': stages, 'commands_execute': False}


def freeze(output, plan):
    commit = rt.output(['git', '-C', rt.GIT_REPOSITORY, 'rev-parse', plan['release']['commit'] + '^{commit}'])
    rt.require(commit == plan['release']['commit'], 'Selected release commit differs')
    archive = subprocess.check_output(['git', '-C', str(rt.GIT_REPOSITORY), 'archive', '--format=tar', commit])
    source = output / 'release-source'
    source.mkdir()
    with tarfile.open(fileobj=io.BytesIO(archive), mode='r:') as stream:
        for member in stream.getmembers():
            name = Path(member.name)
            rt.require(not name.is_absolute() and '..' not in name.parts and
                       (member.isdir() or member.isfile()), 'Unexpected archive member: ' + member.name)
        # Python 3.9 has no filter argument; newer runtimes use the explicit filter.
        stream.extractall(source, **({'filter': 'data'} if hasattr(tarfile, 'data_filter') else {}))
    shutil.copytree(rt.BENCHMARK, output / 'harness/benchmark',
                    ignore=shutil.ignore_patterns('.cache', 'data', 'runs', 'target', '__pycache__', '*.pyc'))
    rt.save(output / 'HARNESS.json', rt.files(output / 'harness/benchmark'))


def resume_index(state, plan):
    rt.require(state['status'] in ('FAILED', 'STOPPED_BY_USER', 'READY'), 'This suite cannot resume in its current state')
    completed = {row['index'] for row in state['attempts'] if row.get('exit') == 0}
    index = next((i for i in range(len(plan['stages'])) if i not in completed), len(plan['stages']))
    rt.require(not any(i > index for i in completed), 'Completed stages are not a consecutive prefix')
    rt.require(index < len(plan['stages']), 'All stages are already complete')
    rt.require(plan['stages'][index]['action'] != 'all',
               'An interrupted/failed performance stage must not be replayed. Retain it and inspect its RUN.json.')
    return index


def execute(args):
    output = args.output.expanduser().resolve()
    if args.action == 'release-resume':
        plan = rt.load(output / 'PLAN.json')
        state = rt.load(output / 'STATE.json')
        index = resume_index(state, plan)
        rt.require(rt.files(output / 'harness/benchmark') == rt.load(output / 'HARNESS.json'),
                   'Frozen harness changed; retain and inspect before resuming')
    else:
        plan = configuration(args)
        plan['commands_execute'] = True
        rt.require(not output.exists(), 'Select a new --output; existing evidence is never overwritten')
        rt.require(not Path(plan['index_root']).exists(), 'Select a new --index-root')
        rt.require(not output.is_relative_to(rt.BENCHMARK) or output.is_relative_to(rt.BENCHMARK / 'runs'),
                   'Place outputs under benchmark/runs or outside benchmark')
        output.mkdir(parents=True)
        rt.save(output / 'PLAN.json', plan)
        state = {'status': 'WAITING_FOR_LOCK', 'started': time.time(), 'attempts': []}
        index = 0
    state.update(status='WAITING_FOR_LOCK', pid=os.getpid())
    save(output / 'STATE.json', state)
    child = None
    def stop(_sig, _frame):
        raise KeyboardInterrupt
    signal.signal(signal.SIGTERM, stop)
    signal.signal(signal.SIGINT, stop)
    try:
        with rt.Lock() as lock:
            save(output / 'LOCK.json', {'state': 'HELD', 'pid': os.getpid(), 'acquired': time.time()})
            try:
                if args.action != 'release-resume':
                    freeze(output, plan)
                context = {'source': str(output / 'release-source'), 'git_repository': str(rt.GIT_REPOSITORY),
                           'cache': str(output / 'program-cache'), 'commit': plan['release']['commit'],
                           'lock_fd': lock.stream.fileno()}
                rt.save(output / 'CONTEXT.json', context)
                env = rt.environment()
                env.update(RLUCENE_BENCHMARK_CONTEXT=str(output / 'CONTEXT.json'),
                           HF_HUB_OFFLINE='1', TOKENIZERS_PARALLELISM='false')
                while index < len(plan['stages']):
                    spec = plan['stages'][index]
                    attempt = 1 + sum(x['index'] == index for x in state['attempts'])
                    name = f"{index:02d}-{spec['action']}-{spec.get('preset', 'suite')}-attempt-{attempt:02d}"
                    command = [sys.executable, '-B', str(output / 'harness/benchmark/vector-search/reproduction/release_stage.py'),
                               str(output), str(index)]
                    row = {'index': index, 'attempt': attempt, 'stage': spec, 'command': command,
                           'started': time.time(), 'stdout': name + '.out', 'stderr': name + '.err'}
                    state['attempts'].append(row)
                    state.update(status='RUNNING', active=spec)
                    save(output / 'STATE.json', state)
                    print('STAGE', spec, flush=True)
                    with (output / row['stdout']).open('xb') as out, (output / row['stderr']).open('xb') as err:
                        child = subprocess.Popen(command, env=env, stdout=out, stderr=err,
                                                 pass_fds=(lock.stream.fileno(),), start_new_session=True)
                        row['pid'] = child.pid
                        save(output / 'STATE.json', state)
                        row['exit'] = child.wait()
                        child = None
                    row['finished'] = time.time()
                    save(output / 'STATE.json', state)
                    rt.require(row['exit'] == 0, 'Stage failed; retained evidence: ' + str(output / row['stderr']))
                    index += 1
                    if args.action == 'release-build':
                        state['status'] = 'READY'
                        break
                else:
                    state['status'] = 'COMPLETE'
            finally:
                if child is not None and child.poll() is None:
                    os.killpg(child.pid, signal.SIGTERM)
                    try:
                        child.wait(timeout=5)
                    except subprocess.TimeoutExpired:
                        os.killpg(child.pid, signal.SIGKILL)
                        child.wait()
                    state['attempts'][-1].update(exit=child.returncode, finished=time.time(), stopped=True)
                save(output / 'LOCK.json', {'state': 'RELEASED', 'pid': os.getpid(), 'released': time.time()})
    except KeyboardInterrupt:
        state.update(status='STOPPED_BY_USER', automatic_resume=False)
        raise
    except BaseException as error:
        state.update(status='FAILED', error=repr(error))
        raise
    finally:
        state['finished'] = time.time()
        save(output / 'STATE.json', state)
    print(state['status'], output, flush=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('action', choices=['release-plan', 'release-build', 'release-run', 'release-resume'])
    parser.add_argument('--output', type=Path)
    parser.add_argument('--data-root', type=Path)
    parser.add_argument('--index-root', type=Path)
    parser.add_argument('--sources', type=Path)
    parser.add_argument('--java-home')
    parser.add_argument('--jars-dir', type=Path)
    parser.add_argument('--inference-python', type=Path)
    parser.add_argument('--download', action='store_true')
    args = parser.parse_args()
    if args.action == 'release-plan':
        print(json.dumps(configuration(args), indent=2))
        return
    rt.require(args.output is not None, 'Select an explicit --output directory')
    if args.action == 'release-resume':
        rt.require(not any((args.data_root, args.index_root, args.sources, args.java_home, args.jars_dir,
                            args.inference_python, args.download)),
                   'release-resume uses the saved plan; supply only --output')
    rt.macos()
    execute(args)


if __name__ == '__main__':
    try:
        main()
    except KeyboardInterrupt:
        sys.exit(130)
