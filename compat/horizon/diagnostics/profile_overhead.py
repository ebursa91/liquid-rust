#!/usr/bin/env python3
"""Verify opt-in profiling overhead on frozen binaries and independent page goldens."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import random
import signal
import statistics
import subprocess
import time


def require(condition, message):
    if not condition:
        raise RuntimeError(message)


def fingerprint(path):
    data = Path(path).read_bytes()
    return {"bytes": len(data), "sha256": hashlib.sha256(data).hexdigest()}


def source_state(root):
    paths = subprocess.check_output(
        ["git", "-C", str(root), "ls-files", "--cached", "--others", "--exclude-standard"],
        text=True).splitlines()
    return {path: fingerprint(root / path) for path in sorted(set(paths))
            if (root / path).is_file()}


def repository(root):
    return {"head": subprocess.check_output(["git", "-C", str(root), "rev-parse", "HEAD"],
                                           text=True).strip(),
            "files": source_state(root)}


def execute(command, directory, timeout):
    with (directory / "stdout.log").open("wb") as stdout, (directory / "stderr.log").open("wb") as stderr:
        process = subprocess.Popen(command, stdout=stdout, stderr=stderr, start_new_session=True)
        try:
            process.wait(timeout=timeout)
            require(process.returncode == 0, f"renderer failed; inspect {directory / 'stderr.log'}")
        finally:
            if process.poll() is None:
                os.killpg(process.pid, signal.SIGKILL)
            process.wait()


def main(args):
    require(args.batches >= 3 and args.iterations >= 20 and args.warmup >= 50,
            "use at least three batches, 50 warmups and 20 measured renders")
    require(sorted(os.sched_getaffinity(0)) == [args.controller_cpu], "pin controller explicitly")
    output = args.output_dir.resolve()
    for root in (args.rust_root.resolve(), args.theme_root.resolve()):
        require(not output.is_relative_to(root) and not root.is_relative_to(output),
                "evidence output must not overlap a source/theme tree")
    manifest = json.loads(args.manifest.read_text())
    require(set(manifest['variants']) == {'feature_off', 'feature_on_inactive'},
            "manifest needs both feature variants")
    off, on = manifest['variants']['feature_off'], manifest['variants']['feature_on_inactive']
    require(off.get('features') == [] and on.get('features') == ['profiling'],
            "manifest must state feature-off and profiling-only activation")
    require(isinstance(off.get('build_flags'), dict) and off['build_flags'] == on.get('build_flags'),
            "overhead comparison requires identical explicit release build flags")
    source = repository(args.rust_root)
    require(source == manifest['source'], "source differs from build manifest")
    inputs = {"manifest": fingerprint(args.manifest), "script": fingerprint(__file__),
              "oracle": fingerprint(args.oracle), "fixture": fingerprint(args.fixture)}
    theme = repository(args.theme_root)
    require(theme['head'] == '5acd1b6b66c02f61d3216e3adace5dd9e0404fc9', "unexpected theme pin")
    require(not subprocess.check_output(["git", "-C", str(args.theme_root), "status", "--porcelain"],
                                        text=True).strip(), "theme is dirty")
    for variant in manifest['variants'].values():
        require(fingerprint(variant['binary']) == variant['fingerprint'], "binary differs from build manifest")
        require(variant['profile'] == 'release', "overhead comparison requires identical release profiles")
    oracle = json.loads(args.oracle.read_text())
    require(set(oracle) == {'html', 'css'}, "oracle needs independent html/css fingerprints")
    args.output_dir.mkdir(parents=True, exist_ok=False)
    rng = random.Random(args.seed)
    runs = []
    load_start = list(os.getloadavg())
    for batch in range(args.batches):
        variants = ['feature_off', 'feature_on_inactive', 'templates', 'detailed']
        rng.shuffle(variants)
        for order, name in enumerate(variants):
            directory = args.output_dir / f"batch-{batch:02d}-{order:02d}-{name}"
            directory.mkdir()
            binary = manifest['variants']['feature_off' if name == 'feature_off' else 'feature_on_inactive']['binary']
            command = ['taskset', '-c', str(args.worker_cpu), binary,
                       '--theme-root', str(args.theme_root), '--fixture', str(args.fixture),
                       '--scope', 'page', '--page', args.page, '--warmup', str(args.warmup),
                       '--iterations', str(args.iterations), '--benchmark-mode', 'direct',
                       '--benchmark-json', str(directory / 'benchmark.json'),
                       '--output-dir', str(directory / 'output')]
            if name in ('templates', 'detailed'):
                command += ['--profile-json', str(directory / 'profile.json'), '--profile-level', name]
            execute(command, directory, args.timeout)
            report = json.loads((directory / 'benchmark.json').read_text())
            require(report['correctness_verified'] and not report['response_cache'], "incorrect benchmark contract")
            require(report['warmup'] == args.warmup and report['iterations'] == args.iterations,
                    "benchmark request counts differ")
            require(report['fixture_sha256'] == inputs['fixture']['sha256'], "fixture differs")
            require(report['theme_sha'] == theme['head'], "fixture theme pin differs")
            samples = report['samples_ms']
            require(len(samples) == args.iterations and all(value >= 0 for value in samples), "invalid timings")
            require(len(report['samples']) == args.iterations, "missing sample digests")
            require(all({key: row[key] for key in oracle} == oracle for row in report['samples']),
                    "measured output differs from independent oracle")
            artifacts = {'html': fingerprint(directory / 'output/index.html'),
                         'css': fingerprint(directory / 'output/styles.css')}
            require(artifacts == oracle, "final output differs from independent oracle")
            profile = None
            if name in ('templates', 'detailed'):
                profile = json.loads((directory / 'profile.json').read_text())
                require(profile.get('schema_version') == 1 and
                        profile.get('clock') == 'monotonic_instrumented_active_wall' and
                        profile.get('operation_succeeded') is True, "invalid/failed profiling export")
                require(profile.get('detailed') is (name == 'detailed'), "wrong profiling level")
                require(profile.get('selected_phases_complete') is True and
                        profile.get('phases', {}).get('measured', {}).get('complete') is True,
                        "measured profiling attribution is incomplete; inspect dropped/balance counters")
                requests = [row for row in profile['aggregates'] if row['phase'] == 'measured'
                            and row['span'] == 'horizon.request']
                require(sum(row['calls'] for row in requests) == args.iterations and
                        sum(row['ok'] for row in requests) == args.iterations,
                        "missing measured request spans/outcomes")
            profile_summary = None
            if profile is not None:
                measured = [row for row in profile['aggregates'] if row['phase'] == 'measured']
                by_span = {}
                for row in measured:
                    totals = by_span.setdefault(row['span'], {key: 0 for key in
                        ('calls', 'ok', 'error', 'owned', 'borrowed', 'inclusive_active_wall_ns',
                         'exclusive_active_wall_ns')})
                    for key in totals:
                        totals[key] += row[key]
                profile_summary = {'fingerprint': fingerprint(directory / 'profile.json'),
                    'raw_profile_path': str(directory / 'profile.json'),
                    'operation_succeeded': profile['operation_succeeded'], 'complete': profile['complete'],
                    'exclusive_valid': profile['exclusive_valid'],
                    'selected_phases_complete': profile['selected_phases_complete'],
                    'counts': profile['counts'], 'phases': profile['phases'], 'limits': profile['limits'],
                    'measured_by_span': by_span,
                    'top_measured_sites_by_exclusive': sorted(measured,
                        key=lambda row: row['exclusive_active_wall_ns'], reverse=True)[:50]}
            runs.append({'batch': batch, 'order': order, 'variant': name,
                         'samples_render_wall_ms': samples, 'median_render_wall_ms': statistics.median(samples),
                         'artifacts': artifacts, 'profile': profile_summary, 'load_after': list(os.getloadavg())})
            print(json.dumps({'batch': batch, 'variant': name, 'median_ms': statistics.median(samples)}), flush=True)
    require(repository(args.rust_root) == source and repository(args.theme_root) == theme,
            "sources changed during experiment")
    require(inputs == {"manifest": fingerprint(args.manifest), "script": fingerprint(__file__),
                       "oracle": fingerprint(args.oracle), "fixture": fingerprint(args.fixture)},
            "inputs changed during experiment")
    for variant in manifest['variants'].values():
        require(fingerprint(variant['binary']) == variant['fingerprint'], "binary changed during experiment")
    summaries = {}
    for name in ('feature_off', 'feature_on_inactive', 'templates', 'detailed'):
        selected = sorted((row for row in runs if row['variant'] == name), key=lambda row: row['batch'])
        medians = [row['median_render_wall_ms'] for row in selected]
        paired = [row['median_render_wall_ms'] / next(base['median_render_wall_ms'] for base in runs
                  if base['variant'] == 'feature_off' and base['batch'] == row['batch']) for row in selected]
        summaries[name] = {'batch_medians_ms': medians, 'median_of_batch_medians_ms': statistics.median(medians),
                           'paired_ratio_to_feature_off': paired}
    result = {'schema_version': 1, 'complete': True, 'correctness_verified': True,
              'source': source, 'theme': theme, 'manifest': manifest, 'inputs': inputs,
              'configuration': {key: getattr(args, key) for key in
                ('page', 'warmup', 'iterations', 'batches', 'seed', 'worker_cpu', 'controller_cpu')},
              'oracle': oracle, 'summary': summaries, 'runs': runs,
              'load_start': load_start, 'load_end': list(os.getloadavg()),
              'limits': ['Internal fresh-render wall time, not CPU, HTTP RPS or RPC latency.',
                         'Measured samples exclude initialization, warmup, hashing, profile export and file writes.',
                         'Active profiling clocks include callback/collector overhead; do not rank engines with them.',
                         'Same release flags; feature activation differs. Default feature-off has no hooks.',
                         'Serial randomized variants on a shared host; affinity does not reserve cores.']}
    (args.output_dir / 'overhead.json').write_text(json.dumps(result, indent=2) + '\n')
    print(json.dumps(summaries), flush=True)


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    for option in ('manifest', 'rust-root', 'theme-root', 'fixture', 'oracle', 'output-dir'):
        parser.add_argument('--' + option, type=Path, required=True)
    parser.add_argument('--page', default='index')
    parser.add_argument('--warmup', type=int, default=50)
    parser.add_argument('--iterations', type=int, default=20)
    parser.add_argument('--batches', type=int, default=3)
    parser.add_argument('--seed', type=int, default=20261005)
    parser.add_argument('--worker-cpu', type=int, default=2)
    parser.add_argument('--controller-cpu', type=int, default=0)
    parser.add_argument('--timeout', type=int, default=600)
    main(parser.parse_args())
