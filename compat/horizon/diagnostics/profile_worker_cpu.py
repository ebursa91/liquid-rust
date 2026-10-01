#!/usr/bin/env python3
"""Bounded-profiler worker CPU overhead; serial verified stdio requests, not HTTP RPS."""
import argparse
import asyncio
import hashlib
import json
import os
from pathlib import Path
import random
import signal
import statistics
import subprocess
import sys
import time


def require(value, message):
    if not value:
        raise RuntimeError(message)


def fingerprint(path):
    content = Path(path).read_bytes()
    return {'bytes': len(content), 'sha256': hashlib.sha256(content).hexdigest()}


def cpu(pid):
    fields = Path(f'/proc/{pid}/stat').read_text().rsplit(')', 1)[1].split()
    ticks = os.sysconf('SC_CLK_TCK')
    return {'user_seconds': int(fields[11]) / ticks, 'system_seconds': int(fields[12]) / ticks}


def validate_ready(ready, args, oracle):
    expected = {'action': 'ready', 'schema_version': 1, 'engine': 'liquid-rust',
                'build_profile': 'release', 'release': True,
                'theme_sha': '5acd1b6b66c02f61d3216e3adace5dd9e0404fc9',
                'scope': 'page', 'page': 'index', 'correctness_verified': True,
                'response_cache': False, 'warmup': args.warmup}
    require(isinstance(ready, dict), 'READY must be an object')
    for key, value in expected.items():
        require(type(ready.get(key)) is type(value) and ready[key] == value,
                'invalid worker READY field ' + key)
    require({key: ready.get(key) for key in ('html', 'css')} == oracle, 'warmup golden differs')
    require(ready.get('fixture_sha256') == fingerprint(args.fixture)['sha256'], 'fixture differs')


async def cleanup_worker(worker):
    # The leader can exit while descendants retain its owned process group.
    try:
        os.killpg(worker.pid, signal.SIGKILL)
    except ProcessLookupError:
        pass
    finally:
        await worker.wait()


async def run(args, name, binary, batch, order, oracle):
    directory = args.output_dir / f'batch-{batch:02d}-{order:02d}-{name}'
    directory.mkdir()
    command = ['taskset', '-c', str(args.worker_cpu), binary, '--theme-root', str(args.theme_root),
               '--fixture', str(args.fixture), '--scope', 'page', '--page', 'index', '--warmup',
               str(args.warmup), '--serve-stdio']
    if name in ('templates', 'detailed'):
        command += ['--profile-json', str(directory / 'profile.json'), '--profile-level', name]
    with (directory / 'stderr.log').open('wb') as log:
        worker = await asyncio.create_subprocess_exec(*command, stdin=asyncio.subprocess.PIPE,
                stdout=asyncio.subprocess.PIPE, stderr=log, start_new_session=True)
        try:
            ready = json.loads(await asyncio.wait_for(worker.stdout.readline(), 300))
            validate_ready(ready, args, oracle)
            require(Path(f'/proc/{worker.pid}/exe').resolve() == Path(binary).resolve(), 'worker executable differs')
            require(sorted(os.sched_getaffinity(worker.pid)) == [args.worker_cpu], 'worker affinity differs')
            before = cpu(worker.pid)
            start = time.perf_counter()
            for identifier in range(args.iterations):
                worker.stdin.write(json.dumps({'action': 'render', 'request_id': identifier}).encode() + b'\n')
                await worker.stdin.drain()
                header = json.loads(await asyncio.wait_for(worker.stdout.readline(), 60))
                require(header['request_id'] == identifier and header['error'] is None, 'worker error')
                for key in ('html', 'css'):
                    expected = oracle[key]
                    require(header[key + '_bytes'] == expected['bytes'], 'response size differs')
                    body = await asyncio.wait_for(worker.stdout.readexactly(expected['bytes']), 60)
                    require(hashlib.sha256(body).hexdigest() == expected['sha256'], 'response hash differs')
            after = cpu(worker.pid)
            elapsed = time.perf_counter() - start
            delta = {key: after[key] - before[key] for key in before}
            worker.stdin.close()
            await asyncio.wait_for(worker.wait(), 60)
            require(worker.returncode == 0, 'worker shutdown/export failed')
            profile = None
            if name in ('templates', 'detailed'):
                data = json.loads((directory / 'profile.json').read_text())
                require(data['operation_succeeded'] and data['selected_phases_complete']
                        and data['phases']['serve']['complete'], 'incomplete active profiling')
                requests = [row for row in data['aggregates'] if row['phase'] == 'serve'
                            and row['span'] == 'horizon.request']
                require(sum(row['calls'] for row in requests) == args.iterations and
                        sum(row['ok'] for row in requests) == args.iterations, 'missing verified request scopes')
                profile = {'fingerprint': fingerprint(directory / 'profile.json'),
                           'counts': data['counts'], 'phases': data['phases'],
                           'selected_phases_complete': data['selected_phases_complete']}
            result = {'batch': batch, 'order': order, 'variant': name, 'iterations': args.iterations,
                      'worker_cpu_seconds': delta, 'worker_cpu_ms_per_request': sum(delta.values()) * 1000 / args.iterations,
                      'verified_pipe_wall_seconds': elapsed, 'profile': profile, 'ready': ready,
                      'load_after': list(os.getloadavg()), 'correctness_verified': True}
            print(json.dumps({key: result[key] for key in ('batch', 'variant', 'worker_cpu_ms_per_request')}), flush=True)
            return result
        finally:
            await cleanup_worker(worker)


async def main(args):
    require(args.batches >= 3 and args.warmup >= 50 and args.iterations >= 100, 'need three batches, W50 and N100')
    require(args.worker_cpu != args.controller_cpu, 'worker and controller CPUs must differ')
    require(sorted(os.sched_getaffinity(0)) == [args.controller_cpu], 'pin controller explicitly')
    for root in (args.rust_root.resolve(), args.theme_root.resolve()):
        require(not args.output_dir.resolve().is_relative_to(root) and
                not root.is_relative_to(args.output_dir.resolve()), 'output overlaps a source tree')
    sys.path.insert(0, str(args.rust_root / 'compat/horizon/diagnostics'))
    import profile_overhead as common
    source = common.repository(args.rust_root)
    theme = common.repository(args.theme_root)
    require(theme['head'] == '5acd1b6b66c02f61d3216e3adace5dd9e0404fc9', 'wrong theme pin')
    require(not subprocess.check_output(['git', '-C', str(args.theme_root), 'status', '--porcelain'],
                                        text=True).strip(), 'dirty theme')
    manifest = json.loads(args.manifest.read_text())
    require(source == manifest['source'], 'source differs from build manifest')
    require(set(manifest['variants']) == {'feature_off', 'feature_on_inactive'}, 'incorrect variants')
    off, on = manifest['variants']['feature_off'], manifest['variants']['feature_on_inactive']
    require(off['features'] == [] and on['features'] == ['profiling'] and
            off['profile'] == on['profile'] == 'release' and off['build_flags'] == on['build_flags'], 'build configurations differ')
    for variant in manifest['variants'].values():
        require(fingerprint(variant['binary']) == variant['fingerprint'], 'binary differs')
    inputs = {'manifest': fingerprint(args.manifest), 'fixture': fingerprint(args.fixture),
              'oracle': fingerprint(args.oracle), 'script': fingerprint(__file__),
              'common_script': fingerprint(args.rust_root / 'compat/horizon/diagnostics/profile_overhead.py')}
    oracle = json.loads(args.oracle.read_text())
    require(set(oracle) == {'html', 'css'}, 'missing oracle artifacts')
    args.output_dir.mkdir(parents=True, exist_ok=False)
    rng, runs = random.Random(args.seed), []
    load_start = list(os.getloadavg())
    for batch in range(args.batches):
        names = ['feature_off', 'feature_on_inactive', 'templates', 'detailed']
        rng.shuffle(names)
        for order, name in enumerate(names):
            binary = manifest['variants']['feature_off' if name == 'feature_off' else 'feature_on_inactive']['binary']
            runs.append(await run(args, name, binary, batch, order, oracle))
    require(common.repository(args.rust_root) == source and common.repository(args.theme_root) == theme,
            'sources changed during experiment')
    for variant in manifest['variants'].values():
        require(fingerprint(variant['binary']) == variant['fingerprint'], 'binary changed')
    require(inputs == {'manifest': fingerprint(args.manifest), 'fixture': fingerprint(args.fixture),
              'oracle': fingerprint(args.oracle), 'script': fingerprint(__file__),
              'common_script': fingerprint(args.rust_root / 'compat/horizon/diagnostics/profile_overhead.py')}, 'inputs changed')
    summary = {}
    for name in ('feature_off', 'feature_on_inactive', 'templates', 'detailed'):
        rows = sorted((row for row in runs if row['variant'] == name), key=lambda row: row['batch'])
        values = [row['worker_cpu_ms_per_request'] for row in rows]
        ratios = [row['worker_cpu_ms_per_request'] / next(base['worker_cpu_ms_per_request'] for base in runs
                  if base['variant'] == 'feature_off' and base['batch'] == row['batch']) for row in rows]
        summary[name] = {'batch_worker_cpu_ms_per_request': values, 'median_worker_cpu_ms_per_request': statistics.median(values),
                         'paired_ratio_to_feature_off': ratios}
    ticks = os.sysconf('SC_CLK_TCK')
    result = {'schema_version': 1, 'complete': True, 'correctness_verified': True, 'manifest': manifest,
              'theme': theme, 'inputs': inputs, 'summary': summary, 'runs': runs, 'oracle': oracle,
              'configuration': {key: getattr(args, key) for key in
                ('batches', 'iterations', 'warmup', 'seed', 'worker_cpu', 'controller_cpu')},
              'load_start': load_start, 'load_end': list(os.getloadavg()), 'clock_ticks_per_second': ticks, 'cpu_tick_seconds': 1 / ticks,
              'limits': ['Worker /proc user+system CPU after READY/warmup through final verified framed response.',
                         'Includes rendering, instrumentation and framing/pipe writes; excludes startup, warmup, parent hashing and profile export.',
                         f'{1000 / ticks:g}ms CPU ticks (SC_CLK_TCK={ticks}) averaged over {args.iterations} requests; not render-only wall time, RPC latency or HTTP RPS.',
                         'Identical release flags; optional profiling changes code generation even with an inactive subscriber.',
                         'All active captures require complete selected-phase attribution. Every response matches independent goldens.',
                         'Serial seeded randomized variants; shared host, affinity does not reserve cores or fix frequency.']}
    (args.output_dir / 'cpu.json').write_text(json.dumps(result, indent=2) + '\n')
    print(json.dumps(summary), flush=True)


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    for option in ('manifest', 'rust-root', 'theme-root', 'fixture', 'oracle', 'output-dir'):
        parser.add_argument('--' + option, type=Path, required=True)
    parser.add_argument('--batches', type=int, default=3)
    parser.add_argument('--warmup', type=int, default=50)
    parser.add_argument('--iterations', type=int, default=100)
    parser.add_argument('--seed', type=int, default=20261006)
    parser.add_argument('--worker-cpu', type=int, default=2)
    parser.add_argument('--controller-cpu', type=int, default=0)
    asyncio.run(main(parser.parse_args()))
