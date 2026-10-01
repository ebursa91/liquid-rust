#!/usr/bin/env python3
"""Capture user-mode CPU samples from an owned, verified Horizon stdio worker."""
import argparse
import asyncio
import hashlib
import json
import os
from pathlib import Path
import signal
import subprocess
import time


def require(value, message):
    if not value:
        raise RuntimeError(message)


def fingerprint(path):
    data = Path(path).read_bytes()
    return {'bytes': len(data), 'sha256': hashlib.sha256(data).hexdigest()}


def tree(root):
    files = subprocess.check_output(['git', '-C', str(root), 'ls-files', '--cached', '--others',
                                     '--exclude-standard'], text=True).splitlines()
    return {name: fingerprint(root / name) for name in sorted(set(files)) if (root / name).is_file()}


async def stop(process):
    if process is None:
        return
    if process.returncode is None:
        try:
            os.killpg(process.pid, signal.SIGKILL)
        except ProcessLookupError:
            pass
    await process.wait()


async def main(args):
    require(args.warmup >= 50 and args.iterations >= 100, 'use 50 warmups and at least 100 requests')
    require(sorted(os.sched_getaffinity(0)) == [args.controller_cpu], 'pin controller explicitly')
    output = args.output_dir.resolve()
    for root in (args.rust_root.resolve(), args.theme_root.resolve()):
        require(not output.is_relative_to(root) and not root.is_relative_to(output),
                'capture output must not overlap a source/theme tree')
    manifest = json.loads(args.manifest.read_text())
    require(tree(args.rust_root) == manifest['source_files'], 'source differs from build manifest')
    require(fingerprint(manifest['binary']) == manifest['binary_fingerprint'], 'binary differs')
    flags = manifest['build_flags']
    require(manifest['profile'] == 'profiling' and '-C force-frame-pointers=yes' in flags.get('RUSTFLAGS', ''),
            'capture needs the profiling build and explicit frame pointers')
    require(manifest['features'] == [], 'native CPU capture must not collect tracing spans')
    inputs = {name: fingerprint(path) for name, path in [('manifest', args.manifest),
              ('fixture', args.fixture), ('sampler', args.sampler), ('sampler_source', args.sampler_source),
              ('driver', Path(__file__))]}
    theme = tree(args.theme_root)
    require(subprocess.check_output(['git', '-C', str(args.theme_root), 'rev-parse', 'HEAD'], text=True).strip()
            == '5acd1b6b66c02f61d3216e3adace5dd9e0404fc9', 'wrong theme pin')
    require(not subprocess.check_output(['git', '-C', str(args.theme_root), 'status', '--porcelain'],
                                        text=True).strip(), 'dirty theme')
    args.output_dir.mkdir(parents=True, exist_ok=False)
    binary = Path(manifest['binary']).resolve()
    process = sampler = None
    started = time.time()
    with (args.output_dir / 'worker.log').open('wb') as log:
        try:
            process = await asyncio.create_subprocess_exec('taskset', '-c', str(args.worker_cpu), str(binary),
                '--theme-root', str(args.theme_root), '--fixture', str(args.fixture), '--scope', 'page',
                '--page', 'index', '--warmup', str(args.warmup), '--serve-stdio', stdin=asyncio.subprocess.PIPE,
                stdout=asyncio.subprocess.PIPE, stderr=log, start_new_session=True)
            ready = json.loads(await asyncio.wait_for(process.stdout.readline(), 300))
            require(ready['correctness_verified'] and not ready['response_cache']
                    and ready['warmup'] == args.warmup, 'invalid warmup contract')
            require(ready['fixture_sha256'] == inputs['fixture']['sha256'], 'different worker fixture')
            require(Path(f'/proc/{process.pid}/exe').resolve() == binary, 'different worker executable')
            require(sorted(os.sched_getaffinity(process.pid)) == [args.worker_cpu], 'different worker affinity')
            (args.output_dir / 'maps.txt').write_text(Path(f'/proc/{process.pid}/maps').read_text())
            sampler = await asyncio.create_subprocess_exec(str(args.sampler), str(process.pid),
                str(args.output_dir / 'cpu.samples'), stdout=asyncio.subprocess.PIPE,
                stderr=asyncio.subprocess.PIPE, start_new_session=True)
            require((await asyncio.wait_for(sampler.stdout.readline(), 10)).strip() == b'ready',
                    'sampler failed; check user-mode perf permission without changing system settings')
            for identifier in range(args.iterations):
                process.stdin.write(json.dumps({'action': 'render', 'request_id': identifier}).encode() + b'\n')
                await process.stdin.drain()
                header = json.loads(await asyncio.wait_for(process.stdout.readline(), 60))
                require(header['request_id'] == identifier and header['error'] is None, 'worker response error')
                for name in ('html', 'css'):
                    require(header[name + '_bytes'] == ready[name]['bytes'], 'response size differs')
                    body = await asyncio.wait_for(process.stdout.readexactly(header[name + '_bytes']), 60)
                    require({'bytes': len(body), 'sha256': hashlib.sha256(body).hexdigest()} == ready[name],
                            'response differs from pinned independent golden')
            sampler.send_signal(signal.SIGTERM)
            _, error = await asyncio.wait_for(sampler.communicate(), 15)
            require(sampler.returncode == 0, 'sampler failed')
            sample_summary = json.loads(error)
            require(sample_summary['lost'] == 0 and sample_summary['samples'] > 0, 'lost or empty samples')
            process.stdin.close()
            await asyncio.wait_for(process.wait(), 30)
            require(process.returncode == 0, 'worker failed on shutdown')
            require(tree(args.rust_root) == manifest['source_files'] and tree(args.theme_root) == theme,
                    'source changed during capture')
            require(fingerprint(binary) == manifest['binary_fingerprint'], 'binary changed during capture')
            require(inputs == {name: fingerprint(path) for name, path in [('manifest', args.manifest),
                ('fixture', args.fixture), ('sampler', args.sampler), ('sampler_source', args.sampler_source),
                ('driver', Path(__file__))]}, 'input changed during capture')
            result = {'schema_version': 1, 'complete': True, 'correctness_verified': True,
                'manifest': manifest, 'inputs': inputs, 'theme_source_files': theme,
                'requests_profiled': args.iterations, 'warmup_excluded': args.warmup,
                'started_unix': started, 'finished_unix': time.time(), 'sample_summary': sample_summary,
                'artifacts': {name: ready[name] for name in ('html', 'css')},
                'capture_files': {name: fingerprint(args.output_dir / name) for name in ('cpu.samples', 'maps.txt')},
                'worker_cpu': args.worker_cpu, 'controller_cpu': args.controller_cpu,
                'limits': ['Owned worker only; user-mode software CPU clock; kernel and off-CPU time excluded.',
                           'Frame-pointer/line-info diagnostic build differs from normal release benchmarks.',
                           'Raw addresses and process maps stay local; inclusive callchain counts overlap.',
                           'Affinity does not reserve cores; scheduling and frequency are uncontrolled.']}
            (args.output_dir / 'metadata.json').write_text(json.dumps(result, indent=2) + '\n')
            print(json.dumps(sample_summary), flush=True)
        finally:
            await stop(sampler)
            await stop(process)


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    for option in ('manifest', 'rust-root', 'theme-root', 'fixture', 'sampler', 'sampler-source', 'output-dir'):
        parser.add_argument('--' + option, type=Path, required=True)
    parser.add_argument('--warmup', type=int, default=50)
    parser.add_argument('--iterations', type=int, default=600)
    parser.add_argument('--worker-cpu', type=int, default=2)
    parser.add_argument('--controller-cpu', type=int, default=0)
    asyncio.run(main(parser.parse_args()))
