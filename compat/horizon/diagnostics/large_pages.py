#!/usr/bin/env python3
"""Isolated, verified warm render-wall comparison for the 100-product Horizon fixture.

This driver never builds or downloads anything. Timings come from the renderer's
internal monotonic render clock, not subprocess elapsed time or reciprocal RPS.
"""

import argparse
import hashlib
import io
import json
import math
import os
import platform
import random
import shutil
import signal
import statistics
import subprocess
import sys
import tarfile
import time
from pathlib import Path

THEME_SHA = "5acd1b6b66c02f61d3216e3adace5dd9e0404fc9"
LIQUID_SHA = "4e39ae4cc3da73921923c0669e0fc84a66b2f696"
BASE_SHA = "a468a7ac18155f98fb9fe51881eafac50bae8105"
DEFAULT_RUBY_BASE = "7db1cc962c1a90a40613da98fdff18f061680bb8"
SCENARIO = "harbor-100-en-default"
PAGES = ("product", "collection")
DEFAULT_BUILD_FLAGS = {
    "CARGO_PROFILE_RELEASE_LTO": None,
    "CARGO_PROFILE_RELEASE_CODEGEN_UNITS": None,
    "RUSTFLAGS": None,
    "CARGO_PROFILE_RELEASE_DEBUG": None,
}


def require(condition, message):
    if not condition:
        raise ValueError(message)


def fingerprint(path):
    digest = hashlib.sha256()
    length = 0
    with Path(path).open("rb") as stream:
        for block in iter(lambda: stream.read(1024 * 1024), b""):
            length += len(block)
            digest.update(block)
    return {"bytes": length, "sha256": digest.hexdigest()}


def git(root, *arguments, binary=False):
    result = subprocess.run(
        ["git", "-C", str(root), *arguments], check=True,
        stdout=subprocess.PIPE, stderr=subprocess.PIPE,
    )
    return result.stdout if binary else result.stdout.decode("utf-8")


def repository(root):
    head = git(root, "rev-parse", "HEAD").strip()
    status = git(root, "status", "--porcelain=v1", "--untracked-files=all")
    require(not status, f"checkout is dirty: {root}")
    files = git(root, "ls-files", "-z").rstrip("\0").split("\0")
    require(files != [""], f"checkout has no tracked files: {root}")
    return {
        "root": str(root), "head": head, "dirty": False,
        "tracked_files": {name: fingerprint(root / name) for name in files},
    }


def base_files(root, revision):
    require(git(root, "rev-parse", revision + "^{commit}").strip() == BASE_SHA,
            "manifest base must resolve to the reviewed a468 commit")
    payload = git(root, "archive", "--format=tar", revision, binary=True)
    result = {}
    with tarfile.open(fileobj=io.BytesIO(payload), mode="r:") as archive:
        for member in archive.getmembers():
            require(not member.issym() and not member.islnk(),
                    "source guard does not accept archived symlinks")
            if member.isfile():
                data = archive.extractfile(member).read()
                result[member.name] = {
                    "bytes": len(data), "sha256": hashlib.sha256(data).hexdigest(),
                }
    require(bool(result), "base archive is empty")
    tracked = set(git(root, "ls-tree", "-r", "--name-only", revision).splitlines())
    require(set(result) == tracked, "Git archive must contain every tracked base file")
    return result


def source_state(variant, expected):
    root = Path(variant["source_root"]).resolve(strict=True)
    files = {name: fingerprint(root / name) for name in expected}
    changes = {name: value["sha256"] for name, value in files.items()
               if value != expected[name]}
    require(changes == variant["changed_source_sha256"],
            f"source differs from the frozen manifest: {variant['name']}: {changes}")
    extras = [str(path.relative_to(root)) for path in root.rglob("*.rs")
              if str(path.relative_to(root)) not in expected and "target" not in path.parts]
    require(not extras, f"unrecorded Rust source files: {variant['name']}: {extras}")
    binary = fingerprint(variant["binary"])
    require(binary["sha256"] == variant["binary_sha256"],
            f"binary differs from manifest: {variant['name']}")
    patch = None
    if variant.get("patch"):
        patch = fingerprint(variant["patch"])
        require(patch["sha256"] == variant["patch_sha256"],
                f"patch differs from manifest: {variant['name']}")
    provenance = None
    if variant.get("build_provenance"):
        provenance = {"fingerprint": fingerprint(variant["build_provenance"]),
                      "report": json.loads(Path(variant["build_provenance"]).read_text())}
        manifest_path = provenance["report"].get("source_manifest_path")
        if manifest_path:
            provenance["source_manifest"] = {
                "path": manifest_path, "fingerprint": fingerprint(manifest_path),
                "rows": json.loads(Path(manifest_path).read_text()),
            }
    return {
        "source_root": str(root), "base_head": BASE_SHA, "source_files": files,
        "changed_source_sha256": changes, "patch": patch, "binary": binary,
        "build_flags": variant["build_flags"], "build_provenance": provenance,
    }


def selected_variants(manifest, args):
    require(manifest.get("base_head") == BASE_SHA, "unexpected manifest base")
    entries = manifest.get("variants")
    require(isinstance(entries, list), "manifest variants must be an array")
    names = [entry.get("name") for entry in entries]
    require(len(names) == len(set(names)), "duplicate manifest variant names")
    by_name = {entry["name"]: entry for entry in entries}
    roles = {"rust_baseline": args.baseline_variant,
             "rust_closest": args.closest_variant,
             "rust_closest_thinlto": args.thinlto_variant}
    require(len(set(roles.values())) == 3, "Rust role selections must be distinct")
    require(all(name in by_name for name in roles.values()), "missing selected variant")
    selected = {role: by_name[name] for role, name in roles.items()}
    for role, variant in selected.items():
        require(Path(variant["binary"]).is_file() and os.access(variant["binary"], os.X_OK),
                f"binary is not executable: {role}")
        require(set(variant["build_flags"]) == set(DEFAULT_BUILD_FLAGS),
                f"build flags must state all four reviewed overrides: {role}")
    require(selected["rust_baseline"]["changed_source_sha256"] == {},
            "baseline must have no changes from a468")
    require(selected["rust_baseline"]["build_flags"] == DEFAULT_BUILD_FLAGS and
            selected["rust_closest"]["build_flags"] == DEFAULT_BUILD_FLAGS,
            "baseline and closest must use the recorded default release build")
    expected_thin = dict(DEFAULT_BUILD_FLAGS, CARGO_PROFILE_RELEASE_LTO="thin",
                         CARGO_PROFILE_RELEASE_CODEGEN_UNITS="1")
    require(selected["rust_closest_thinlto"]["build_flags"] == expected_thin,
            "ThinLTO variant must record thin LTO and one codegen unit only")
    require(selected["rust_closest"].get("patch_sha256") and
            selected["rust_closest"]["patch_sha256"] ==
            selected["rust_closest_thinlto"].get("patch_sha256"),
            "closest builds must use the same frozen patch")
    return selected


def verify_thin_provenance(state):
    require(state["build_provenance"] is not None, "ThinLTO build provenance is required")
    report = state["build_provenance"]["report"]
    require(report.get("base_head") == BASE_SHA and
            report.get("changed_source_sha256") == state["changed_source_sha256"] and
            report.get("build_flags") == state["build_flags"] and
            report.get("build_success") is True and
            report.get("source_verified_after_build") is True and
            report.get("all_tracked_files_verified") is True and
            report.get("production_instrumentation") is False,
            "ThinLTO provenance does not bind the reviewed uninstrumented source/build")
    require(report.get("tracked_file_count") == len(state["source_files"]),
            "ThinLTO tracked-file count differs")
    require({key: report["binary"][key] for key in ("bytes", "sha256")} == state["binary"],
            "ThinLTO provenance executable differs")
    require(report.get("cargo_lock_sha256") == state["source_files"]["Cargo.lock"]["sha256"],
            "ThinLTO Cargo.lock differs")
    require(report.get("patch", {}).get("sha256") == state["patch"]["sha256"],
            "ThinLTO provenance patch differs")
    if report.get("source_manifest_path"):
        rows = state["build_provenance"]["source_manifest"]["rows"]
        require(isinstance(rows, list) and len(rows) == len(state["source_files"]),
                "ThinLTO source manifest rows differ")
        canonical = json.dumps(rows, sort_keys=True, separators=(",", ":")).encode("utf-8")
        require(hashlib.sha256(canonical).hexdigest() == report.get("source_manifest_sha256"),
                "ThinLTO canonical source manifest digest differs")
        row_paths = [row["path"] for row in rows]
        require(len(row_paths) == len(set(row_paths)), "duplicate ThinLTO manifest paths")
        require({row["path"]: {key: row[key] for key in ("bytes", "sha256")} for row in rows} ==
                state["source_files"], "ThinLTO source manifest does not bind every current file")


def read_goldens(report, fixture):
    require(report.get("schema_version") == 1 and report.get("correctness_verified") is True,
            "offline oracle report is not successful")
    require(report.get("theme_sha") == THEME_SHA and report.get("liquid_sha") == LIQUID_SHA,
            "offline oracle uses different pins")
    cases = report.get("cases")
    require(isinstance(cases, list), "offline oracle cases must be an array")
    result = {}
    for page in PAGES:
        matching = [case for case in cases
                    if case.get("scenario_id") == SCENARIO and case.get("page") == page]
        require(len(matching) == 1, f"oracle needs one {SCENARIO}/{page} case")
        case = matching[0]
        require(case["fixture"] == fixture and case["product_count"] == 100 and
                case["configuration"] == "default" and
                case["observable_page"]["locale"] == "en" and
                case["observable_page"]["current_page"] == 1 and
                case["template"] == f"templates/{page}.json" and
                case["repeated_renders_per_engine"] >= 2,
                f"oracle fixture/page contract differs: {page}")
        result[page] = case
    return result


def percentile(values, percent):
    ordered = sorted(values)
    position = (len(ordered) - 1) * percent / 100.0
    lower = math.floor(position)
    upper = math.ceil(position)
    return ordered[lower] + (ordered[upper] - ordered[lower]) * (position - lower)


def finite_number(value, label, positive=False):
    require(type(value) in (int, float) and math.isfinite(value) and
            (value > 0 if positive else value >= 0), f"invalid {label}: {value!r}")


def validate_report(report, role, page, golden, args, ruby_state):
    require(report.get("schema_version") == 1 and report.get("correctness_verified") is True and
            report.get("response_cache") is False and report.get("scope") == "page" and
            report.get("page") == page and report.get("warmup") == args.warmup and
            report.get("iterations") == args.iterations and
            report.get("fixture_sha256") == golden["fixture"]["sha256"] and
            report.get("theme_sha") == THEME_SHA, f"worker contract differs: {role}/{page}")
    expected = golden["artifacts"]
    require({key: report.get(key) for key in ("html", "css")} == expected,
            f"top-level worker digests differ from oracle: {role}/{page}")
    samples = report.get("samples")
    times = report.get("samples_ms")
    require(isinstance(samples, list) and isinstance(times, list) and
            len(samples) == args.iterations and len(times) == args.iterations,
            "wrong number of measured samples")
    for index, (sample, elapsed) in enumerate(zip(samples, times)):
        require(isinstance(sample, dict), "sample must be an object")
        finite_number(elapsed, f"sample {index} elapsed_ms", positive=True)
        require(sample.get("elapsed_ms") == elapsed and
                {key: sample.get(key) for key in ("html", "css")} == expected,
                f"sample digest/time differs: {role}/{page}/{index}")
    finite_number(report.get("initialization_ms"), "initialization_ms")
    if role == "ruby_yjit":
        require(report.get("engine") == "ruby" and report.get("execution_mode") == "direct" and
                report.get("ruby_version") == args.ruby_version and
                report.get("yjit_enabled") is True and report.get("gc_enabled") is True and
                report.get("clock") == "CLOCK_MONOTONIC" and
                report.get("liquid_sha") == LIQUID_SHA and report.get("source_dirty") is False and
                report.get("source_sha") == ruby_state["head"], "Ruby runtime/source differs")
        loaded = Path(report["liquid_source"]).resolve(strict=True)
        require(loaded.is_relative_to(args.liquid_root), "Ruby did not load the pinned Liquid checkout")
        finite_number(report.get("first_render_ms"), "first_render_ms")
        for key in ("samples_cpu_ms", "samples_allocations"):
            values = report.get(key)
            require(isinstance(values, list) and len(values) == args.iterations,
                    f"invalid Ruby {key}")
            for index, value in enumerate(values):
                finite_number(value, f"{key}[{index}]")
                sample_key = "cpu_ms" if key == "samples_cpu_ms" else "allocations"
                require(samples[index].get(sample_key) == value, f"Ruby {key} disagrees with samples")
    else:
        require(report.get("engine") == "liquid-rust" and report.get("release") is True and
                report.get("build_profile") == "release" and
                report.get("benchmark_mode") == "direct" and
                report.get("timer") == "std::time::Instant monotonic wall clock",
                "Rust runtime/build differs")
        require(report.get("samples_cpu_ms") is None, "unexpected Rust CPU measurement")
        require(report.get("runtime") == {"package_version": "0.26.11", "os": "linux",
                                           "architecture": "x86_64"},
                "Rust package/runtime metadata differs")
        diagnostics = report.get("diagnostics", {})
        require(diagnostics.get("error") is None and diagnostics.get("page") == page and
                diagnostics.get("locale") == "en", "Rust diagnostic page/locale/error differs")
    return times


def checked_command(command, output_dir, timeout, worker_cpu, env):
    log = output_dir / "worker.log"
    process = subprocess.Popen(command, stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
                               env=env, start_new_session=True)
    captured = b""
    affinity = None
    started = time.monotonic()
    try:
        # taskset has a brief startup interval on the controller's inherited CPU.
        deadline = started + min(5.0, timeout)
        while time.monotonic() < deadline:
            if process.poll() is not None:
                break
            try:
                current = sorted(os.sched_getaffinity(process.pid))
            except ProcessLookupError:
                break
            if current == [worker_cpu]:
                affinity = current
                break
            time.sleep(0.005)
        require(affinity == [worker_cpu], "could not verify the assigned worker affinity")
        try:
            captured, _ = process.communicate(timeout=max(0.001, timeout - (time.monotonic() - started)))
        except subprocess.TimeoutExpired:
            try:
                os.killpg(process.pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
            captured, _ = process.communicate()
            raise TimeoutError(f"worker exceeded {timeout} seconds")
        require(process.returncode == 0, f"worker failed with status {process.returncode}")
        return affinity
    finally:
        # Kill the owned process group; communicate() reaps the direct worker.
        try:
            os.killpg(process.pid, signal.SIGKILL)
        except ProcessLookupError:
            pass
        tail, _ = process.communicate()
        # communicate() returns the same complete stdout on subsequent calls.
        if tail:
            captured = tail
        log.write_bytes(captured)


def safe_output_path(path, protected):
    absolute = path.absolute()
    for node in (absolute, *absolute.parents):
        require(not node.is_symlink(), f"output path has a symlink: {node}")
    resolved = absolute.resolve()
    require(not resolved.exists(), "output directory must be new; previous successes are never reused")
    for source in protected:
        source = source.resolve()
        require(not resolved.is_relative_to(source) and not source.is_relative_to(resolved),
                f"output directory overlaps protected source/input: {source}")
    return resolved


def summarize(runs, roles, batches):
    output = {}
    ratios = {}
    for page in PAGES:
        output[page] = {}
        by_batch = {}
        for role in roles:
            selected = sorted((run for run in runs if run["page"] == page and run["variant"] == role),
                              key=lambda run: run["batch"])
            require(len(selected) == batches, "missing result window")
            pooled = [value for run in selected for value in run["samples_render_wall_ms"]]
            medians = [run["median_render_wall_ms"] for run in selected]
            by_batch[role] = medians
            output[page][role] = {
                "measured_samples": len(pooled), "median_render_wall_ms": statistics.median(pooled),
                "p95_render_wall_ms": percentile(pooled, 95), "p99_render_wall_ms": percentile(pooled, 99),
                "min_render_wall_ms": min(pooled), "max_render_wall_ms": max(pooled),
                "batch_median_render_wall_ms": medians,
                "batch_median_min_ms": min(medians), "batch_median_max_ms": max(medians),
            }
        ratios[page] = {}
        for role in roles:
            if role != "rust_baseline":
                values = [baseline / candidate for baseline, candidate in
                          zip(by_batch["rust_baseline"], by_batch[role])]
                ratios[page][f"rust_baseline_over_{role}"] = {
                    "batch_median_ratios": values, "median": statistics.median(values),
                    "min": min(values), "max": max(values),
                }
    return output, ratios


def main(args):
    for name in ("manifest", "ruby", "ruby_root", "ruby_gem_home", "rust_root", "liquid_root",
                 "theme_root", "fixture", "oracle_report"):
        setattr(args, name, getattr(args, name).resolve(strict=True))
    require(sys.platform == "linux" and hasattr(os, "sched_getaffinity"), "Linux affinity is required")
    require(shutil.which("taskset") is not None, "taskset is required")
    require(args.batches >= 3 and args.iterations >= 20 and args.warmup >= 50,
            "publication comparison requires >=3 batches, >=20 samples and >=50 warmups")
    require(math.isfinite(args.timeout) and args.timeout > 0, "timeout must be finite and positive")
    require(args.worker_cpu != args.controller_cpu and args.worker_cpu >= 0 and args.controller_cpu >= 0,
            "controller and renderer must use separate nonnegative CPUs")
    require(sorted(os.sched_getaffinity(0)) == [args.controller_cpu],
            "launch the driver with taskset on --controller-cpu")
    manifest = json.loads(args.manifest.read_text())
    variants = selected_variants(manifest, args)
    expected = base_files(args.rust_root, manifest["base_head"])
    states = {role: source_state(variant, expected) for role, variant in variants.items()}
    require(states["rust_closest"]["source_files"] == states["rust_closest_thinlto"]["source_files"],
            "default and ThinLTO closest sources must be byte-identical")
    verify_thin_provenance(states["rust_closest_thinlto"])
    repositories = {role: repository(root) for role, root in
                    (("ruby", args.ruby_root), ("liquid", args.liquid_root), ("theme", args.theme_root))}
    require(repositories["liquid"]["head"] == LIQUID_SHA and
            repositories["theme"]["head"] == THEME_SHA, "external checkout pins differ")
    changed_ruby = git(args.ruby_root, "diff", "--name-only", args.ruby_base, "HEAD").splitlines()
    forbidden = [name for name in changed_ruby if name.startswith(("lib/", "bin/", "test/", "proto/", "fixtures/"))
                 or name in ("Gemfile", "Gemfile.lock") or (name.startswith("benchmark/") and name.endswith(".rb"))]
    require(not forbidden, f"Ruby implementation differs from reviewed baseline: {forbidden}")
    input_paths = {"fixture": args.fixture, "ruby_executable": args.ruby, "script": Path(__file__).resolve(),
                   "manifest": args.manifest, "oracle_report": args.oracle_report}
    inputs = {name: fingerprint(path) for name, path in input_paths.items()}
    golden_report = json.loads(args.oracle_report.read_text())
    goldens = read_goldens(golden_report, inputs["fixture"])
    fixture = json.loads(args.fixture.read_text())
    require(fixture.get("manifest", {}).get("scenario", {}).get("id") == SCENARIO or
            fixture.get("manifest", {}).get("scenario_id") == SCENARIO,
            "fixture does not declare the genuine harbor-100-en-default scenario")
    protected = [args.rust_root, args.ruby_root, args.liquid_root, args.theme_root,
                 *(Path(variant["source_root"]) for variant in variants.values()), *input_paths.values(),
                 *(Path(variant["binary"]) for variant in variants.values())]
    args.output_dir = safe_output_path(args.output_dir, protected)
    args.output_dir.mkdir(parents=True, exist_ok=False)
    env = os.environ.copy()
    for key in ("RUBYLIB", "RUBYOPT", "RUBY_YJIT_ENABLE", "GEM_HOME", "GEM_PATH", "BUNDLE_PATH",
                "BUNDLE_GEMFILE", "HORIZON_DATA_TOKEN", "HORIZON_STORE_TOKEN",
                "HORIZON_PROFILE_ALLOCATIONS", "HORIZON_PROFILE_HOST"):
        env.pop(key, None)
    for key in tuple(env):
        if key.startswith(("RUBY_YJIT_", "BUNDLE_")):
            env.pop(key)
    env.update(BUNDLE_GEMFILE=str(args.ruby_root / "Gemfile"),
               GEM_HOME=str(args.ruby_gem_home), GEM_PATH=str(args.ruby_gem_home))
    # Persist only selected nonsecret environment settings, never os.environ.
    runtime_environment = {key: env[key] for key in ("BUNDLE_GEMFILE", "GEM_HOME", "GEM_PATH")}
    roles = [*variants, "ruby_yjit"]
    started = time.time()
    load_start = list(os.getloadavg())
    rng = random.Random(args.seed)
    runs = []
    try:
        for batch in range(args.batches):
            windows = [(page, role) for page in PAGES for role in roles]
            rng.shuffle(windows)
            for order, (page, role) in enumerate(windows):
                if role in variants:
                    require(source_state(variants[role], expected) == states[role],
                            f"source/binary drift before {role}")
                else:
                    require(repository(args.ruby_root) == repositories["ruby"], "Ruby drift before worker")
                require(fingerprint(args.fixture) == inputs["fixture"], "fixture drift before worker")
                directory = args.output_dir / f"batch-{batch:02d}-{order:02d}-{page}-{role}"
                directory.mkdir()
                shared = ["--theme-root", str(args.theme_root), "--fixture", str(args.fixture),
                          "--scope", "page", "--page", page, "--warmup", str(args.warmup),
                          "--iterations", str(args.iterations), "--output-dir", str(directory),
                          "--benchmark-json", str(directory / "benchmark.json")]
                if role == "ruby_yjit":
                    command = [str(args.ruby), "--yjit", "-rbundler/setup",
                               str(args.ruby_root / "benchmark/worker.rb"),
                               "--liquid-root", str(args.liquid_root), "--benchmark-mode", "direct", *shared]
                else:
                    command = [variants[role]["binary"], "--benchmark-mode", "direct", *shared]
                command = ["taskset", "-c", str(args.worker_cpu), *command]
                load_before = list(os.getloadavg())
                affinity = checked_command(command, directory, args.timeout, args.worker_cpu, env)
                load_after = list(os.getloadavg())
                report = json.loads((directory / "benchmark.json").read_text())
                times = validate_report(report, role, page, goldens[page], args, repositories["ruby"])
                actual = {"html": fingerprint(directory / "index.html"),
                          "css": fingerprint(directory / "styles.css")}
                require(actual == goldens[page]["artifacts"], f"actual output differs: {role}/{page}")
                if role in variants:
                    require(source_state(variants[role], expected) == states[role],
                            f"source/binary drift after {role}")
                else:
                    require(repository(args.ruby_root) == repositories["ruby"], "Ruby drift after worker")
                runs.append({
                    "variant": role, "manifest_variant": variants[role]["name"] if role in variants else None,
                    "page": page, "batch": batch, "order": order, "command": command,
                    "output_dir": str(directory), "worker_affinity": affinity,
                    "warmup": args.warmup, "iterations": args.iterations,
                    "samples_render_wall_ms": times, "median_render_wall_ms": statistics.median(times),
                    "p95_render_wall_ms": percentile(times, 95), "p99_render_wall_ms": percentile(times, 99),
                    "min_render_wall_ms": min(times), "max_render_wall_ms": max(times),
                    "load_before": load_before, "load_after": load_after, "artifacts": actual,
                    "worker_report": report, "worker_report_fingerprint": fingerprint(directory / "benchmark.json"),
                    "correctness_verified": True,
                })
                (args.output_dir / "progress.json").write_text(json.dumps({
                    "complete": False, "verified_windows": len(runs), "runs": runs,
                }, indent=2) + "\n")
                print(f"batch {batch} {page} {role}: internal render-wall median "
                      f"{statistics.median(times):.3f} ms", flush=True)
        require(inputs == {name: fingerprint(path) for name, path in input_paths.items()}, "input drift")
        require(states == {role: source_state(variant, expected) for role, variant in variants.items()},
                "final source/binary/patch drift")
        require(repositories == {role: repository(Path(state["root"])) for role, state in repositories.items()},
                "final external source/Ruby drift")
        verify_thin_provenance(states["rust_closest_thinlto"])
        require(sorted(os.sched_getaffinity(0)) == [args.controller_cpu], "controller affinity changed")
        summary, ratios = summarize(runs, roles, args.batches)
        result = {
            "schema_version": 1, "complete": True, "correctness_verified": True,
            "started_unix": started, "completed_unix": time.time(),
            "configuration": {key: getattr(args, key) for key in
                              ("batches", "warmup", "iterations", "seed", "worker_cpu", "controller_cpu",
                               "timeout", "ruby_version", "ruby_base")},
            "scenario_id": SCENARIO, "pages": list(PAGES), "scope": "page",
            "measurement": "worker internal monotonic render-wall samples in milliseconds",
            "variant_roles": {role: variant["name"] for role, variant in variants.items()},
            "sources": states, "other_sources": repositories, "inputs": inputs,
            "runtime_environment": runtime_environment, "oracle_cases": goldens,
            "machine": {"platform": platform.platform(), "architecture": platform.machine(),
                        "python": platform.python_version(), "logical_cpus": os.cpu_count(),
                        "controller_affinity": sorted(os.sched_getaffinity(0))},
            "load_start": load_start, "load_end": list(os.getloadavg()),
            "verified_measured_outputs": len(runs) * args.iterations,
            "excluded_verified_warmup_outputs": len(runs) * args.warmup,
            "verified_final_artifacts": len(runs), "summary": summary,
            "paired_batch_median_ratios": ratios, "runs": runs,
            "limits": [
                "Internal render-wall includes fresh request/runtime/diagnostic state and HTML/CSS assembly. "
                "Rust Instant and Ruby CLOCK_MONOTONIC stop before per-result SHA checks and artifact/report writes.",
                "Renderer initialization, excluded warmups, process startup, Bundler/Git loading, outer verification "
                "and process cleanup are outside these samples. No whole-command CPU or wall timing is reported.",
                "Each worker retains parsed/source caches but freshly renders every request; response_cache=false. "
                "Workers hash every warmup/measurement; the driver checks every measured digest and final HTML/CSS "
                "bytes against the independently published pinned Ruby/Rust offline parity oracle.",
                "Three or more seeded randomized serial batches on an uncontrolled shared host. CPU affinity "
                "does not reserve a core or control clock frequency; load and batch ranges are retained.",
                "Percentiles pool measured samples using linear interpolation. Batch ratios and ranges are "
                "descriptive; a small number of fresh-process batches does not establish universal rankings.",
                "ThinLTO uses the exact closest source with thin LTO AND codegen-units=1; any change belongs to "
                "that combined build configuration. Manifest/source hashes bind inputs, not a signed build attestation.",
                "This is an offline synthetic full-page workload, not live gRPC fetch, HTTP service throughput "
                "or RPS. Do not invert render medians into an RPS claim.",
            ],
        }
        (args.output_dir / "comparison.json").write_text(json.dumps(result, indent=2) + "\n")
        print(json.dumps(summary, indent=2), flush=True)
    except BaseException as error:
        (args.output_dir / "failure.json").write_text(json.dumps({
            "complete": False, "correctness_verified": False,
            "error": f"{type(error).__name__}: {error}", "verified_windows": len(runs),
        }, indent=2) + "\n")
        raise


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    for option in ("manifest", "ruby", "ruby-root", "ruby-gem-home", "rust-root", "liquid-root", "theme-root",
                   "fixture", "oracle-report", "output-dir"):
        parser.add_argument("--" + option, type=Path, required=True)
    parser.add_argument("--baseline-variant", default="rust_baseline")
    parser.add_argument("--closest-variant", default="rust_host_closest")
    parser.add_argument("--thinlto-variant", default="rust_host_closest_thinlto")
    parser.add_argument("--ruby-base", default=DEFAULT_RUBY_BASE)
    parser.add_argument("--ruby-version", default="4.0.7")
    parser.add_argument("--batches", type=int, default=3)
    parser.add_argument("--iterations", type=int, default=20)
    parser.add_argument("--warmup", type=int, default=50)
    parser.add_argument("--seed", type=int, default=20261004)
    parser.add_argument("--worker-cpu", type=int, default=2)
    parser.add_argument("--controller-cpu", type=int, default=0)
    parser.add_argument("--timeout", type=float, default=1800.0)
    main(parser.parse_args())
