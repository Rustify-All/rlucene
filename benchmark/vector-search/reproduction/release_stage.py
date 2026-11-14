"""One serial release stage; uses the measured drivers and sampling unchanged."""
from collections import Counter
from pathlib import Path
import json
import shutil
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[2]))
from common import runtime as rt
import build
import runner


def derive(source, target, config):
    full = runner.preset('minilm-wiki-384-m16-1m')
    runner.inputs(source, full)
    rt.require(not target.exists(), 'Partial derivation retained; inspect or select a new data root: ' + str(target))
    target.mkdir(parents=True)
    remaining = config['documents'] * config['dimensions'] * 4
    with (source / 'base.raw').open('rb') as stream, (target / 'base.raw').open('xb') as output:
        while remaining:
            block = stream.read(min(8 * 1024 * 1024, remaining))
            rt.require(bool(block), 'Truncated full MiniLM input')
            output.write(block)
            remaining -= len(block)
    for name in ('query.raw', 'queries.txt'):
        if (source / name).exists():
            shutil.copyfile(source / name, target / name)
    observed = runner.inputs(target, config)
    rt.save(target / 'DATASET.json', {'complete': True, 'config': config, 'vectors': observed,
            'derived_from': str(source),
            'method': 'First 500000 original-order document rows; identical complete query vectors; no normalization'})


def audit(root, plan):
    receipt = build.programs(plan['java_home'])
    rt.require(receipt['ordinary_release'] and receipt['source']['commit'] == plan['release']['commit'] and
               not receipt['source']['modified_library_files'], 'Release source identity differs')
    workloads = []
    summary = ['# Alpha.3 vector release comparison', '',
               'Rust time change relative to Java, percent; negative is faster.', '',
               '|Preset|Build batch1 / batch2|fanout0 batch1 / batch2|fanout100 batch1 / batch2|',
               '|---|---:|---:|---:|']
    for config in plan['configurations']:
        name = config['id']
        path = Path(plan['index_root']) / name
        run, result = rt.load(path / 'RUN.json'), rt.load(path / 'RESULT.json')
        rt.require(run['status'] == 'COMPLETE' and result['complete'] and result['standard_full_run'],
                   'Incomplete release workload: ' + name)
        rt.require(run['config'] == config and run['build'] == receipt, 'Workload identity changed: ' + name)
        runner.inputs(Path(plan['data_root']) / name, config, truth=True)
        rt.require(all(run['inputs']['files'][n] == config['input_files'][n]
                       for n in ('base.raw', 'query.raw', 'groundtruth.raw')), 'Published input/truth differs')
        rt.require(len(run['builds']) == 4 and run['compatibility']['complete'] and
                   len(run['compatibility']['indices']) == 4, 'Incomplete build or compatibility evidence')
        rt.require(all(x['vector_f32_bits_equal_to_original'] and x['stored_ID_doc_ord_mapping_equal'] and
                       x['canonical_equal_between_readers'] and x['integrity_both_readers']
                       for x in run['compatibility']['indices']), 'Structural compatibility failed')
        rt.require({x['fanout'] for x in result['profiles']} == {0, 100}, 'Missing query profile')
        for profile in run['profiles']:
            rt.require(len(profile['batches']) == 2, 'Missing batch')
            for batch in profile['batches']:
                rt.require(batch['complete'] and len(batch['rounds']) == 12 and
                           Counter(tuple(x['order']) for x in batch['rounds']) ==
                           {('rust', 'java'): 6, ('java', 'rust'): 6}, 'Incomplete or unbalanced rounds')
        for profile in result['profiles']:
            for batch in profile['batches']:
                rt.require(batch['complete'] and batch['complete_rounds'] == 12 and
                           all(x['warmup']['ns'] >= 60_000_000_000 for x in batch['engines'].values()),
                           'Actual warmup or complete rounds missing')
        retained = root / 'results' / name
        if not retained.exists():
            retained.parent.mkdir(exist_ok=True)
            shutil.copytree(path, retained, ignore=shutil.ignore_patterns('indices'))
        original = {p.relative_to(path).as_posix(): {'bytes': p.stat().st_size, 'sha256': rt.sha(p)}
                    for p in path.rglob('*') if p.is_file() and 'indices' not in p.relative_to(path).parts}
        rt.require(rt.files(retained) == original, 'Retained result copy differs; preserve and inspect')
        workloads.append({'preset': name, 'evidence': str(retained), 'RUN_sha256': rt.sha(path / 'RUN.json'),
                          'RESULT_sha256': rt.sha(path / 'RESULT.json'),
                          'build_comparisons': result['build_comparisons'],
                          'query_profiles': result['profiles'], 'compatibility': run['compatibility']})
        builds = ' / '.join(f"{x['rust_time_percent']:.3f}%" for x in result['build_comparisons'])
        queries = {p['fanout']: ' / '.join(f"{b['rust_time_percent']:.3f}%" for b in p['batches'])
                   for p in result['profiles']}
        summary.append(f"|{name}|{builds}|{queries[0]}|{queries[100]}|")
    rt.save(root / 'FINAL_AUDIT.json', {'complete': True, 'release': plan['release'],
            'all_observations_retained': True, 'workloads': workloads,
            'actual_rlucene_metadata': receipt['actual_rlucene_metadata'],
            'library_modified_in_frozen_copy': receipt['library_modified'],
            'configuration_patch_sha256': receipt['codec_patch_sha256']})
    summary += ['', 'See each RESULT.json, per-query.csv and differences/ for recall, percentiles, CPU, RSS,',
                'index compatibility, graph, score-bit, ordering and visited differences. Wiki report policy',
                'does not assert cross-writer graph or score-bit equality. SIFT remains strict.',
                'All samples, regressions and failed attempts are retained; this command does not publish a website.']
    (root / 'SUMMARY.md').write_text('\n'.join(summary) + '\n')


def main():
    root = Path(sys.argv[1])
    plan = rt.load(root / 'PLAN.json')
    stage = plan['stages'][int(sys.argv[2])]
    action = stage['action']
    with rt.Lock():
        if action == 'build':
            build.build(plan['java_home'], plan['jars_dir'], build.SUITE / plan['release']['cargo_lock'])
            return
        if action == 'audit':
            audit(root, plan)
            return
        name = stage['preset']
        config = runner.preset(name)
        data = Path(plan['data_root']) / name
        if action in ('prepare', 'derive') and all((data / n).is_file() for n in ('base.raw', 'query.raw')):
            runner.inputs(data, config)
            print('PINNED_VECTORS_REUSED', data)
            return
        if action == 'derive':
            derive(Path(plan['data_root']) / 'minilm-wiki-384-m16-1m', data, config)
            return
        if action == 'truth' and (data / 'groundtruth.raw').exists():
            runner.inputs(data, config, truth=True)
            print('VALIDATED_TRUTH_REUSED', data)
            return
        command = ['prepare-data' if action == 'prepare' else action, '--preset', name, '--data', str(data)]
        if plan['java_home']:
            command += ['--java-home', plan['java_home']]
        if action == 'prepare':
            command += ['--sources', plan['sources']]
            if plan['download']:
                command.append('--download')
            if plan['inference_python']:
                command += ['--inference-python', plan['inference_python']]
        if action == 'all':
            command += ['--output', str(Path(plan['index_root']) / name)]
        sys.argv = ['reproduce.py', *command]
        runner.main()


if __name__ == '__main__':
    main()
