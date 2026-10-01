#!/usr/bin/env python3
"""Benchmark independent pinned Horizon renderers after validating every output."""
import argparse
from datetime import datetime, timezone
import hashlib
import json
import math
import os
from pathlib import Path
import platform
import random
import shutil
import signal
import statistics
import subprocess
import sys
import time

LIQUID_SHA = "4e39ae4cc3da73921923c0669e0fc84a66b2f696"
THEME_SHA = "5acd1b6b66c02f61d3216e3adace5dd9e0404fc9"
VARIANTS = ("rust", "ruby", "ruby_yjit", "ruby_fiber_yjit")
ARTIFACTS = {"html": "index.html", "css": "styles.css"}


def require(condition, message):
    if not condition:
        raise ValueError(message)


def digest_bytes(data):
    return {"bytes": len(data), "sha256": hashlib.sha256(data).hexdigest()}


def fingerprint(path):
    require(path.is_file() and not path.is_symlink(), f"expected a regular file: {path}")
    return digest_bytes(path.read_bytes())


def git(root, *args):
    return subprocess.check_output(["git", "-C", str(root), *args])


def repository(root, expected=None):
    top = Path(os.fsdecode(git(root, "rev-parse", "--show-toplevel")).strip()).resolve()
    require(top == root, f"repository root required: {root}")
    head = git(root, "rev-parse", "HEAD").decode().strip()
    status = git(root, "status", "--porcelain=v1", "--untracked-files=all").decode()
    require(expected is None or (head == expected and not status), f"clean checkout at {expected} required: {root}")
    tracked = git(root, "ls-files", "-z").split(b"\0")
    untracked = git(root, "ls-files", "--others", "--exclude-standard", "-z").split(b"\0")
    hasher = hashlib.sha256()
    for name in sorted(set(tracked + untracked) - {b""}):
        path = root / os.fsdecode(name)
        hasher.update(name + b"\0")
        if path.is_symlink():
            hasher.update(b"link\0" + os.fsencode(os.readlink(path)))
        elif path.is_file():
            hasher.update(b"file\0" + path.read_bytes())
        else:
            hasher.update(b"missing\0")
        hasher.update(b"\0")
    return {"path": str(root), "head": head, "dirty": bool(status), "status": status,
            "source_sha256": hasher.hexdigest(), "diff_sha256": hashlib.sha256(git(root, "diff", "--binary", "HEAD")).hexdigest()}


def safe_directory(path, output_root):
    require(not path.is_symlink(), f"output directory must not be a symlink: {path}")
    if path != output_root:
        for parent in path.parents:
            if parent == output_root:
                break
            require(not parent.is_symlink(), f"output ancestor must not be a symlink: {parent}")
    path.mkdir(parents=True, exist_ok=True)
    require(path.resolve().is_relative_to(output_root), f"output escaped benchmark directory: {path}")
    return path


def fresh_output(path, output_root):
    safe_directory(path, output_root)
    for name in ("index.html", "styles.css", "report.json", "benchmark.json", "process.log", "rss.txt"):
        target = path / name
        require(not target.is_symlink(), f"output file must not be a symlink: {target}")
        require(not target.exists() or target.is_file(), f"output file is not regular: {target}")
        target.unlink(missing_ok=True)


def host():
    cpu_model = None
    cpuinfo = Path("/proc/cpuinfo")
    if cpuinfo.exists():
        cpu_model = next((line.split(":", 1)[1].strip() for line in cpuinfo.read_text().splitlines() if line.startswith("model name")), None)
    return {"platform": platform.platform(), "machine": platform.machine(), "logical_cpus": os.cpu_count(),
            "cpu_model": cpu_model, "available_cpus": sorted(os.sched_getaffinity(0)) if hasattr(os, "sched_getaffinity") else None,
            "load_average": list(os.getloadavg()) if hasattr(os, "getloadavg") else None}


def finite_time(value, label):
    require(type(value) in (int, float) and math.isfinite(value) and value >= 0, f"invalid {label}: {value!r}")
    return float(value)


def quantile(values, fraction):
    values = sorted(values)
    position = (len(values) - 1) * fraction
    lower, upper = math.floor(position), math.ceil(position)
    return values[lower] + (values[upper] - values[lower]) * (position - lower)


def interval(values, seed):
    if len(values) < 3:
        return None
    rng = random.Random(seed)
    samples = [statistics.median(rng.choices(values, k=len(values))) for _ in range(10000)]
    return [quantile(samples, 0.025), quantile(samples, 0.975)]


def describe(windows, seed):
    samples = [sample for window in windows for sample in window]
    medians = [statistics.median(window) for window in windows]
    return {"unit": "ms", "samples": len(samples), "batch_windows": len(windows),
            "median_ms": statistics.median(samples), "p95_ms": quantile(samples, 0.95),
            "batch_medians_ms": medians, "median_batch_ms": statistics.median(medians),
            "batch_median_min_ms": min(medians), "batch_median_max_ms": max(medians),
            "batch_median_mad_ms": statistics.median(abs(v - statistics.median(medians)) for v in medians),
            "median_batch_bootstrap_95_interval_ms": interval(medians, seed)}


def artifact_digests(output, expected):
    actual = {key: fingerprint(output / name) for key, name in ARTIFACTS.items()}
    require(expected is None or actual == expected, f"HTML/CSS differs from pinned Ruby baseline: {output}")
    return actual


def validate_report(report, variant, args, fixture_hash, warm, expected):
    require(report.get("fixture_sha256") == fixture_hash, f"{variant}: worker consumed a different fixture")
    require(report.get("theme_sha") == THEME_SHA, f"{variant}: worker theme pin differs")
    if variant != "rust":
        require(report.get("liquid_sha") == LIQUID_SHA, f"{variant}: Ruby Liquid pin differs")
        require(report.get("yjit_enabled") is (variant != "ruby"), f"{variant}: actual YJIT state differs")
        mode = "fiber" if variant == "ruby_fiber_yjit" else "direct"
        require(report.get("execution_mode") == mode, f"{variant}: execution mode differs")
        require(report.get("ruby_version") == args.expected_ruby_version, f"{variant}: Ruby runtime version differs")
        dependencies = report.get("dependencies")
        require(isinstance(dependencies, dict) and {"bigdecimal", "strscan"}.issubset(dependencies), f"{variant}: actual Ruby dependency metadata missing")
        for name, dependency in dependencies.items():
            require(isinstance(dependency, dict) and isinstance(dependency.get("version"), str) and isinstance(dependency.get("path"), str), f"{variant}: invalid dependency metadata for {name}")
        baseline_dependencies = getattr(args, "expected_dependencies", None)
        require(baseline_dependencies is None or dependencies == baseline_dependencies, f"{variant}: Ruby dependency versions/paths changed")
        args.expected_dependencies = dependencies
    if not warm:
        return None
    require(report.get("schema_version") == 1 and report.get("correctness_verified") is True, f"{variant}: invalid benchmark contract")
    require(report.get("iterations") == args.iterations and report.get("warmup") == args.warmup, f"{variant}: iteration counts differ")
    require(report.get("scope") == args.scope and report.get("page") == args.page, f"{variant}: scope/page differs")
    require(report.get("response_cache") is False, f"{variant}: response caching must be disabled")
    if variant == "rust":
        require(report.get("build_profile") == "release", "Rust benchmark requires a release binary")
    samples = report.get("samples")
    times = report.get("samples_ms")
    require(isinstance(samples, list) and isinstance(times, list) and len(samples) == len(times) == args.iterations, f"{variant}: incomplete samples")
    for index, (sample, elapsed) in enumerate(zip(samples, times)):
        finite_time(elapsed, "sample duration")
        require(finite_time(sample.get("elapsed_ms"), "sample duration") == elapsed, f"{variant}: sample timings disagree")
        require({key: sample.get(key) for key in ARTIFACTS} == expected, f"{variant}: iteration {index} output differs")
    require({key: report.get(key) for key in ARTIFACTS} == expected, f"{variant}: final worker digests differ")
    for key in ("samples_cpu_ms", "samples_allocations"):
        if report.get(key) is not None:
            require(isinstance(report[key], list) and len(report[key]) == args.iterations, f"{variant}: incomplete {key}")
            for value in report[key]:
                finite_time(value, key)
    return times


def execute(command, output, args, env):
    fresh_output(output, args.output_dir)
    if args.cpu is not None:
        command = [args.taskset, "--cpu-list", str(args.cpu), *command]
    if args.measure_rss:
        command = ["/usr/bin/time", "--format=%M", "--output", str(output / "rss.txt"), "--", *command]
    load_before = host()["load_average"]
    start = time.perf_counter_ns()
    process = subprocess.Popen(command, cwd=args.rust_root, env=env, stdout=subprocess.PIPE,
                               stderr=subprocess.STDOUT, start_new_session=os.name == "posix")
    try:
        captured, _ = process.communicate(timeout=args.timeout)
    except subprocess.TimeoutExpired:
        # Include descendants such as Ruby's Git checks and the optional RSS wrapper.
        try:
            if os.name == "posix":
                os.killpg(process.pid, signal.SIGKILL)
            else:
                process.kill()
        except ProcessLookupError:
            pass
        captured, _ = process.communicate()
        (output / "process.log").write_bytes(captured)
        raise ValueError(f"renderer timed out after {args.timeout}s; see {output / 'process.log'}") from None
    elapsed = (time.perf_counter_ns() - start) / 1e6
    # communicate drains stdout/EOF instead of wait(timeout)'s polling sleep.
    # Persisting the captured log is outside the end-to-end timing boundary.
    (output / "process.log").write_bytes(captured)
    require(process.returncode == 0, f"renderer exited {process.returncode}; see {output / 'process.log'}")
    rss = None
    if args.measure_rss:
        rss = int((output / "rss.txt").read_text().strip())
    return {"command": command, "end_to_end_ms": elapsed, "load_before": load_before, "load_after": host()["load_average"],
            "process_peak_rss_kib": rss, "output_dir": str(output)}


def command(variant, output, args, warm=False):
    shared = ["--theme-root", str(args.theme_root), "--fixture", str(args.fixture), "--scope", args.scope, "--output-dir", str(output)]
    if variant == "rust":
        cmd = [str(args.rust_binary), *shared]
        if warm:
            cmd += ["--benchmark-json", str(output / "benchmark.json"), "--iterations", str(args.iterations), "--warmup", str(args.warmup), "--benchmark-mode", "direct"]
        return cmd
    mode = "fiber" if variant == "ruby_fiber_yjit" else "direct"
    script = args.ruby_root / ("benchmark/worker.rb" if warm else "bin/horizon-render")
    cmd = [str(args.ruby), "--disable-yjit" if variant == "ruby" else "--yjit", "-rbundler/setup", str(script), "--liquid-root", str(args.liquid_root), "--page", args.page, *shared]
    cmd += ["--benchmark-mode" if warm else "--mode", mode]
    if warm:
        cmd += ["--iterations", str(args.iterations), "--warmup", str(args.warmup)]
    return cmd


def arguments():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ("ruby-root", "liquid-root", "theme-root", "rust-root", "rust-binary", "fixture", "output-dir"):
        parser.add_argument("--" + name, required=True, type=Path)
    parser.add_argument("--ruby", default="ruby", help="Ruby executable, including an absolute version-managed path")
    parser.add_argument("--ruby-gem-home", type=Path, help="Optional dependency cache for the selected Ruby version")
    parser.add_argument("--ruby-gem-path", help="Optional colon-separated gem search path for the selected Ruby version")
    parser.add_argument("--scope", choices=("hero", "template", "page"), default="page")
    parser.add_argument("--page", default="index")
    parser.add_argument("--iterations", type=int, default=30)
    parser.add_argument("--warmup", type=int, default=50)
    parser.add_argument("--batches", type=int, default=7)
    parser.add_argument("--cold-iterations", type=int, default=3)
    parser.add_argument("--seed", type=int, default=20261001)
    parser.add_argument("--cpu", type=int, help="Optional allowed Linux CPU; affinity does not isolate hardware")
    parser.add_argument("--measure-rss", action="store_true", help="Linux GNU time process peak RSS; its startup is included equally in cold timings")
    parser.add_argument("--timeout", type=float, default=900, help="Maximum seconds per process")
    parser.add_argument("--expected-comparison", type=Path, help="Optional successful compare.py manifest for additional expected digest verification")
    args = parser.parse_args()
    for name in ("ruby_root", "liquid_root", "theme_root", "rust_root", "rust_binary", "fixture", "output_dir"):
        value = getattr(args, name)
        require(name != "output_dir" or not value.is_symlink(), "benchmark output directory must not be a symlink")
        setattr(args, name, value.resolve())
    for root in (args.ruby_root, args.rust_root, args.liquid_root, args.theme_root):
        require(not args.output_dir.is_relative_to(root), f"generated artifacts must be outside source checkout: {root}")
    return args


def main():
    args = arguments()
    ruby = shutil.which(args.ruby)
    inputs = [args.fixture, args.rust_binary]
    if ruby:
        inputs.append(Path(ruby).resolve())
    if args.expected_comparison:
        args.expected_comparison = args.expected_comparison.resolve()
        inputs.append(args.expected_comparison)
    for path in inputs:
        require(not path.is_relative_to(args.output_dir), f"benchmark output must not contain an input: {path}")
    safe_directory(args.output_dir, args.output_dir)
    for name in ("benchmark.json", "summary.txt"):
        target = args.output_dir / name
        require(not target.is_symlink(), f"success output must not be a symlink: {target}")
        target.unlink(missing_ok=True)
    require(args.iterations > 0 and args.batches > 0 and args.cold_iterations > 0 and args.warmup >= 1 and math.isfinite(args.timeout) and args.timeout > 0, "invalid counts or timeout")
    require(ruby is not None, f"Ruby executable missing: {args.ruby}")
    args.ruby = Path(ruby).resolve()
    require(args.rust_binary.is_file() and os.access(args.rust_binary, os.X_OK), "executable Rust release binary required")
    require(args.page == "index", "Rust fixture worker currently supports the index page")
    if args.cpu is not None:
        args.taskset = shutil.which("taskset")
        require(platform.system() == "Linux" and args.taskset and hasattr(os, "sched_getaffinity") and args.cpu in os.sched_getaffinity(0), "requested CPU affinity is unavailable")
    if args.measure_rss:
        require(platform.system() == "Linux" and Path("/usr/bin/time").is_file(), "GNU time RSS measurement requires Linux /usr/bin/time")
    fixture = json.loads(args.fixture.read_text())
    require(fixture.get("schema_version") == 1 and fixture.get("synthetic") is True and fixture.get("theme", {}).get("sha") == THEME_SHA, "pinned synthetic fixture required")
    sources = {"ruby": repository(args.ruby_root), "rust": repository(args.rust_root), "liquid": repository(args.liquid_root, LIQUID_SHA), "theme": repository(args.theme_root, THEME_SHA)}
    fixture_digest = fingerprint(args.fixture)
    binary_digest = fingerprint(args.rust_binary)
    ruby_digest = fingerprint(args.ruby)
    public_fixture_digest = fingerprint(args.ruby_root / "fixtures/store.json")
    require(public_fixture_digest == fixture_digest, "public Ruby fixture differs from --fixture bytes")
    validator = args.rust_root / "compat/horizon/validate_fixture.py"
    require(validator.is_file(), f"fixture validator missing: {validator}")
    subprocess.run([sys.executable, str(validator), str(args.fixture)], check=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
    env = os.environ.copy()
    removed_env = ("RUBYLIB", "RUBYOPT", "RUBY_YJIT_ENABLE", "GEM_HOME", "GEM_PATH", "BUNDLE_PATH")
    for key in removed_env:
        env.pop(key, None)
    if args.ruby_gem_home:
        env["GEM_HOME"] = str(args.ruby_gem_home.resolve())
    if args.ruby_gem_path:
        env["GEM_PATH"] = args.ruby_gem_path
    ruby_version = subprocess.check_output([str(args.ruby), "--disable-yjit", "--version"], text=True, env=env).strip()
    args.expected_ruby_version = ruby_version.split()[1]
    # Dependencies must already be installed. Never run Bundler/Cargo in a timed process.
    env["BUNDLE_GEMFILE"] = str(args.ruby_root / "Gemfile")
    started = datetime.now(timezone.utc).isoformat()
    machine_start = host()
    expected = None
    if args.expected_comparison:
        comparison = json.loads(args.expected_comparison.read_text())
        require(comparison.get("fixture") == fixture_digest and comparison.get("scope") == args.scope and comparison.get("theme_sha") == THEME_SHA and comparison.get("ruby_liquid_sha") == LIQUID_SHA, "comparison manifest does not match benchmark inputs")
        expected = {key: comparison["artifacts"][name] for key, name in ARTIFACTS.items()}
    preflight = []
    for variant in ("ruby", "ruby_yjit", "ruby_fiber_yjit", "rust"):
        output = args.output_dir / "preflight" / variant
        record = execute(command(variant, output, args), output, args, env)
        actual = artifact_digests(output, expected)
        expected = expected or actual
        report = json.loads((output / "report.json").read_text())
        validate_report(report, variant, args, fixture_digest["sha256"], False, expected)
        record.update(variant=variant, report=report)
        preflight.append(record)
    rng = random.Random(args.seed)
    runs = []
    for batch in range(args.batches):
        # Round order and engine order are randomized without concurrent measured work.
        rounds = [("cold", index) for index in range(args.cold_iterations)] + [("warm", 0)]
        rng.shuffle(rounds)
        for phase, round_index in rounds:
            order = list(VARIANTS)
            rng.shuffle(order)
            for position, variant in enumerate(order):
                output = args.output_dir / phase / f"batch-{batch:03d}" / f"round-{round_index:03d}" / variant
                warm = phase == "warm"
                record = execute(command(variant, output, args, warm), output, args, env)
                artifact_digests(output, expected)
                report_path = output / ("benchmark.json" if warm and variant == "rust" else "report.json")
                fingerprint(report_path)
                report = json.loads(report_path.read_text())
                samples = validate_report(report, variant, args, fixture_digest["sha256"], warm, expected)
                record.update(variant=variant, phase=phase, batch=batch, round=round_index, order=position, samples_ms=samples, report=report)
                runs.append(record)
                print(f"{phase} batch {batch + 1}/{args.batches}: {variant}", file=sys.stderr, flush=True)
        require(fingerprint(args.fixture) == fixture_digest and fingerprint(args.rust_binary) == binary_digest, "fixture or binary changed during benchmark")
    paths = {"ruby": args.ruby_root, "rust": args.rust_root, "liquid": args.liquid_root, "theme": args.theme_root}
    require(fingerprint(args.ruby) == ruby_digest, "Ruby executable changed during benchmark")
    require(fingerprint(args.ruby_root / "fixtures/store.json") == public_fixture_digest, "public Ruby fixture changed during benchmark")
    require({key: repository(path, LIQUID_SHA if key == "liquid" else THEME_SHA if key == "theme" else None) for key, path in paths.items()} == sources, "source checkout changed during benchmark")
    summary = {}
    for phase in ("cold", "warm"):
        summary[phase] = {}
        for variant in VARIANTS:
            windows = []
            for batch in range(args.batches):
                selected = [run for run in runs if run["phase"] == phase and run["variant"] == variant and run["batch"] == batch]
                windows.append([run["end_to_end_ms"] for run in selected] if phase == "cold" else selected[0]["samples_ms"])
            summary[phase][variant] = describe(windows, args.seed)
            summary[phase][variant]["process_count"] = args.batches * (args.cold_iterations if phase == "cold" else 1)
        summary[phase]["paired_ruby_over_rust_ratios"] = {}
        rust_medians = summary[phase]["rust"]["batch_medians_ms"]
        for variant in VARIANTS[1:]:
            ratios = [ruby / rust for ruby, rust in zip(summary[phase][variant]["batch_medians_ms"], rust_medians)]
            summary[phase]["paired_ruby_over_rust_ratios"][variant] = {"batch_ratios": ratios, "median_ratio": statistics.median(ratios), "bootstrap_95_interval": interval(ratios, args.seed)}
    result = {"schema_version": 1, "correctness_verified": True, "started_at_utc": started, "completed_at_utc": datetime.now(timezone.utc).isoformat(),
              "configuration": {key: getattr(args, key) for key in ("iterations", "warmup", "batches", "cold_iterations", "seed", "cpu", "measure_rss", "scope", "page")},
              "fixture": fixture_digest, "artifacts": expected, "sources": sources,
              "rust_binary": {"path": str(args.rust_binary), **binary_digest, "source_binding": "operator-supplied --rust-root; binary digest recorded independently"},
              "ruby_executable": {"path": str(args.ruby), "version": ruby_version, **ruby_digest},
              "environment": {"removed_from_inherited": list(removed_env), "bundle_gemfile": env["BUNDLE_GEMFILE"], "gem_home": env.get("GEM_HOME"), "gem_path": env.get("GEM_PATH")},
              "host_start": machine_start, "host_end": host(), "preflight": preflight, "runs": runs, "summary": summary,
              "statistics_note": "Descriptive percentile bootstrap across independent process/batch medians; shared hardware, scheduler, GC/JIT and system load are not controlled. Affinity is not isolation."}
    lines = [f"Ruby: {ruby_version}", f"Fixture: {fixture_digest['sha256']}", "All independently rendered HTML/CSS outputs validated.", "Times in ms; descriptive batch bootstrap intervals; shared host, no CPU isolation."]
    for phase in ("cold", "warm"):
        lines.append("Cold fresh-process end-to-end" if phase == "cold" else "Warm reused-AST render-only (fresh request contexts)")
        for variant in VARIANTS:
            stats = summary[phase][variant]
            lines.append(f"  {variant}: median={stats['median_ms']:.3f}, p95={stats['p95_ms']:.3f}, batch medians={stats['batch_medians_ms']}, median-batch 95% interval={stats['median_batch_bootstrap_95_interval_ms']}")
    # The JSON success marker is published last; a failed run cannot retain one.
    (args.output_dir / "summary.txt").write_text("\n".join(lines) + "\n")
    (args.output_dir / "benchmark.json").write_text(json.dumps(result, indent=2, allow_nan=False) + "\n")
    print("\n".join(lines))


if __name__ == "__main__":
    try:
        main()
    except (ValueError, OSError, KeyError, TypeError, json.JSONDecodeError, subprocess.SubprocessError) as error:
        sys.exit(str(error))
