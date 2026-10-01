#!/usr/bin/env python3
"""Temporary fixed-fixture render timer and whole-command CPU diagnostic."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import random
import resource
import statistics
import subprocess
import time

EXPECTED = {
    "html": {"bytes": 435304, "sha256": "d97c35b3ba08f026536cb4c469623acb9957af59fb9a9171db012958515fe990"},
    "css": {"bytes": 279112, "sha256": "67a6538e0b763c32ced001728ebf68f375dec497d4fb91c0ee136658ff9f2034"},
}
FIXTURE_SHA = "867c41e0929881290af2b261af64146632bf0287524f5a6c55714af32e819f98"
THEME_SHA = "5acd1b6b66c02f61d3216e3adace5dd9e0404fc9"


def fingerprint(path):
    data = Path(path).read_bytes()
    return {"bytes": len(data), "sha256": hashlib.sha256(data).hexdigest()}


def run(name, binary, args, batch, order):
    directory = args.output_dir / f"batch-{batch:02d}-{name}"
    command = ["taskset", "-c", str(args.worker_cpu), str(binary), "--theme-root", str(args.theme_root),
               "--fixture", str(args.fixture), "--page", "index", "--scope", "page", "--output-dir", str(directory),
               "--benchmark-json", str(directory / "benchmark.json"), "--warmup", str(args.warmup), "--iterations", str(args.iterations)]
    before = resource.getrusage(resource.RUSAGE_CHILDREN)
    start = time.perf_counter()
    completed = subprocess.run(command, stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=180, check=True)
    elapsed = time.perf_counter() - start
    after = resource.getrusage(resource.RUSAGE_CHILDREN)
    (directory / "stderr.log").write_bytes(completed.stderr)
    report = json.loads((directory / "benchmark.json").read_text())
    assert json.loads(completed.stdout) == report
    assert report["build_profile"] == "release" and report["release"] is True
    assert report["engine"] == "liquid-rust" and report["scope"] == "page" and report["page"] == "index"
    assert report["fixture_sha256"] == FIXTURE_SHA and report["theme_sha"] == THEME_SHA
    assert report["correctness_verified"] is True and report["response_cache"] is False
    assert report["warmup"] == args.warmup and report["iterations"] == args.iterations
    assert len(report["samples"]) == len(report["samples_ms"]) == args.iterations
    assert {key: report[key] for key in EXPECTED} == EXPECTED
    for sample, milliseconds in zip(report["samples"], report["samples_ms"]):
        assert sample["elapsed_ms"] == milliseconds and milliseconds >= 0
        assert {key: sample[key] for key in EXPECTED} == EXPECTED
    assert fingerprint(directory / "index.html") == EXPECTED["html"]
    assert fingerprint(directory / "styles.css") == EXPECTED["css"]
    assert report["diagnostics"]["error"] is None and len(report["diagnostics"]["sources"]) == 117
    profiles = [json.loads(line.removeprefix("HORIZON_REGEX_PROFILE ")) for line in completed.stderr.decode().splitlines()
                if line.startswith("HORIZON_REGEX_PROFILE ")]
    assert len(profiles) == (1 if name == "instrumentation" else 0)
    result = {"variant": name, "batch": batch, "order": order, "correctness_verified": True,
              "samples_render_wall_ms": report["samples_ms"], "median_render_wall_ms": statistics.median(report["samples_ms"]),
              "initialization_ms": report["initialization_ms"], "process_wall_seconds": elapsed,
              "whole_command_user_seconds": after.ru_utime - before.ru_utime,
              "whole_command_system_seconds": after.ru_stime - before.ru_stime,
              "whole_command_cpu_scope": "includes process startup, fixture/setup, 50 warmups, 30 measured renders, all digest checks and artifact/JSON writes; excludes controller hashing",
              "load_after": list(os.getloadavg()), "regex_profile": profiles[0] if profiles else None}
    print(name, batch, "render median ms", result["median_render_wall_ms"], "user", result["whole_command_user_seconds"],
          "system", result["whole_command_system_seconds"], flush=True)
    return result


def main(args):
    assert args.warmup == 50 and args.iterations == 30 and args.batches >= 3
    args.output_dir.mkdir(parents=True, exist_ok=False)
    binaries = {"baseline": args.baseline, "cache": args.candidate, "instrumentation": args.instrumentation}
    inputs = {name: fingerprint(binary) for name, binary in binaries.items()}
    inputs["fixture"] = fingerprint(args.fixture)
    assert inputs["fixture"]["sha256"] == FIXTURE_SHA
    assert len({inputs[name]["sha256"] for name in binaries}) == 3
    rng = random.Random(args.seed)
    results = []
    started = time.time()
    load_start = list(os.getloadavg())
    for batch in range(args.batches):
        variants = ["baseline", "cache"]
        rng.shuffle(variants)
        for order, name in enumerate(variants):
            results.append(run(name, binaries[name], args, batch, order))
    results.append(run("instrumentation", binaries["instrumentation"], args, args.batches, 0))
    assert inputs == {**{name: fingerprint(binary) for name, binary in binaries.items()}, "fixture": fingerprint(args.fixture)}
    summary = {}
    for name in ["baseline", "cache"]:
        medians = [result["median_render_wall_ms"] for result in results if result["variant"] == name]
        summary[name] = {"batch_median_render_wall_ms": medians, "median_of_batch_medians_ms": statistics.median(medians)}
    output = {"schema_version": 1, "correctness_verified": True, "started_unix": started, "completed_unix": time.time(),
              "configuration": {"batches": args.batches, "warmup": args.warmup, "iterations": args.iterations,
                                "worker_cpu": args.worker_cpu, "controller_affinity": sorted(os.sched_getaffinity(0)), "seed": args.seed},
              "inputs": inputs, "artifacts": EXPECTED, "theme_sha": THEME_SHA, "load_start": load_start, "load_end": list(os.getloadavg()),
              "summary": summary, "runs": results,
              "limits": ["Warm internal Instant wall timers exclude digest checks and artifact writes.",
                         "Whole-command child user/system CPU includes startup, setup, warmup, measured renders and all digest/artifact writes; it is not render-only CPU.",
                         "Instrumentation counts/times actual Regex::new only, across the whole process. Absolute timing includes its clock/counter overhead.",
                         "One instrumentation process supplies attribution; its overall render time is diagnostic, not used in baseline/cache comparison.",
                         "Fixed-seed serial randomized variants, CPU affinity not reservation; shared load/frequency remain uncontrolled."]}
    (args.output_dir / "warm-ab.json").write_text(json.dumps(output, indent=2) + "\n")
    print(json.dumps(summary), flush=True)


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    for argument in ["baseline", "candidate", "instrumentation", "theme-root", "fixture", "output-dir"]:
        parser.add_argument("--" + argument, type=Path, required=True)
    parser.add_argument("--warmup", type=int, default=50)
    parser.add_argument("--iterations", type=int, default=30)
    parser.add_argument("--batches", type=int, default=3)
    parser.add_argument("--worker-cpu", type=int, default=2)
    parser.add_argument("--seed", type=int, default=20261001)
    main(parser.parse_args())
