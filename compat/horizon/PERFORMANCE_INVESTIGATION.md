# Horizon performance investigation, 2026-10-01

The configured Rust host was slower than Ruby YJIT on this synthetic homepage. Reusing two fixed regular expressions cut observed Rust worker CPU from **68.5 to 31.5 ms/request**, a **54.0% reduction**. Ruby 4.0.7 YJIT measured **24.2 ms/request** in the same probe and remains ahead. These results compare the complete configured hosts and their Shopify adapters; they do not rank Rust, Ruby or Liquid implementations generally.

| Variant | Median batch-average worker CPU ms/request | Batch-average range | Three batch averages |
| --- | ---: | ---: | --- |
| Rust baseline | 68.5 | 67.8–80.6 | 68.5, 67.8, 80.6 |
| Rust with regex reuse | 31.5 | 30.3–32.1 | 30.3, 32.1, 31.5 |
| Ruby 4.0.7 YJIT | 24.2 | 22.7–26.5 | 26.5, 22.7, 24.2 |

The [original numeric CPU samples](results/2026-10-01-cpu-investigation.json) and [regex profile/provenance](results/2026-10-01-regex-profile.json) retain the measurement boundaries and source/input fingerprints. The table reports the median of three independent **batch averages**, not a median of per-request CPU samples. The cached Rust host used about 30% more worker CPU than YJIT in this probe. The residual cost has not been fully attributed.

## What changed

The adapter compiled the same expression on every settings-binding and translation-interpolation call. Separate instrumentation counted **41 settings calls and 83 translation calls per render: 124 compilations**. Across 80 requests and one additional initialization call, it observed 9,921 compilations. Compilation timers totaled 3,125.91 ms in that diagnostic process; its clocks/counters and whole-process boundary differ from the CPU probe, so the absolute time is attribution evidence rather than another benchmark result.

The fix retains the two existing patterns in immutable `LazyLock` values. It preserves Unicode matching, whitespace, missing-value behavior and replacement semantics. It caches compiled expressions; each request still renders fresh HTML/CSS. [Commit a468a7a](https://github.com/ebursa91/liquid-rust/commit/a468a7ac18155f98fb9fe51881eafac50bae8105) changes only `examples/horizon.rs`, including regression tests. It changes no core Liquid API. The measured candidate was isolated and uncommitted at measurement time; its exact source subsequently became this commit.

Baseline: `63df2051d8501a80b764681f7c797ea0907ba718`. Candidate source SHA256: `c18388f257fea77a421c0de4878746b995f130cb532a4e13a7b5227fa41e21cc`. Measured baseline binary SHA256: `25a7b14b8b807039f2b900f03ffb8d8c7703a61ff48bc92bff9176c08fca2ea7`; candidate binary: `598f23d126440e402ce5911ee10fd48efe79f360bba8c5427395f73c40f14e94`. Rebuilds need their own binary fingerprints.

A separate diagnostic found owned subtree copies while presenting option-value `.size`. The two instrumented lookup sites were small and variable, and did not measure all lookup/projection costs or explain the remaining CPU difference. No lookup API change followed from that probe. Keep further optimization tied to measured costs and exact-output checks.

## Method and limits

Three batches ran the baseline, cache candidate and Ruby YJIT serially in a fixed-seed randomized order. Every process excluded 50 independently checked warmup requests, then handled 100 measured requests: **450 warmup and 900 measured renders**. Workers retained parsed sources/ASTs and created fresh request state; they cached no rendered response. Every measured response was checked against the independent original Ruby oracle. All outputs matched exactly.

The CPU interval starts after the worker announces readiness and ends after its final response has been received and verified. It uses Linux `/proc/pid/stat` user plus system CPU. Worker CPU includes rendering, JSON framing and pipe writes; it excludes startup, warmup and the controller's SHA256 CPU. Counters have 10 ms ticks, averaged over 100 requests per batch. This is **worker CPU per request**, not render-only wall latency or HTTP RPS. Per-response pipe wall additionally includes transport and controller verification.

The controller ran on CPU 0 and workers on CPU 2 of the shared Linux i9-9900K workstation. Affinity does not reserve cores, control frequency or remove sibling/system load. Initial/final one-minute load was 7.38/4.93. Rust/Cargo was 1.98.1; both Rust workers were release builds. Ruby was 4.0.7 with actual YJIT enabled. Source and input fingerprints were unchanged across the experiment.

An additional internal render-wall comparison excluded hashing/artifact writes, but its three baseline medians varied 66.04, 86.85 and 117.68 ms; cache medians were 49.31, 47.70 and 37.92 ms. Those shared-host measurements support the direction of the improvement, not a precise wall-time ratio. They use different boundaries and sample counts from the CPU probe.

The workload is the original four-product offline store and unmodified external Horizon homepage. It includes no gRPC data fetch, real customer data or commerce service. The separate [HTTP rerun](RPS_RESULTS.md) measured this committed fix with transport calibration and full error/drain accounting. CPU means cannot be inverted into RPS, and the separately executed HTTP runs do not establish a precise causal speedup.

Oracle bindings:

- Fixture: 772031 bytes, SHA256 `867c41e0929881290af2b261af64146632bf0287524f5a6c55714af32e819f98`.
- HTML: 435304 bytes, SHA256 `d97c35b3ba08f026536cb4c469623acb9957af59fb9a9171db012958515fe990`.
- Collected CSS: 279112 bytes, SHA256 `67a6538e0b763c32ced001728ebf68f375dec497d4fb91c0ee136658ff9f2034`.
- Horizon: `5acd1b6b66c02f61d3216e3adace5dd9e0404fc9`; Ruby Liquid: `4e39ae4cc3da73921923c0669e0fc84a66b2f696`; Ruby host: `7db1cc962c1a90a40613da98fdff18f061680bb8`.

## Reproduce the original CPU probe

Use Rust/Cargo 1.98.1, install the locked dependencies and prepare pinned external checkouts before timing. Set the variables below to separate source checkouts and an evidence directory outside them. `CPU_PROBE` should point to [diagnostics/worker_cpu.py](diagnostics/worker_cpu.py), the original orchestration script, SHA256 `a3930b2e3ddb5b8b4c66720a41d2520cca294454650f3598aab1ecde6c8683c9`. The script checks clean inputs, pins, release/JIT state, exact response hashes and source/binary stability. It expects the candidate's original measurement state: baseline HEAD with only the regex patch applied. A clean checkout directly at the cached commit will fail that guard.

```bash
BASE_SHA=63df2051d8501a80b764681f7c797ea0907ba718
CACHE_SHA=a468a7ac18155f98fb9fe51881eafac50bae8105
mkdir -p "$EVIDENCE_DIR"
git -C "$RUST_REPO" worktree add --detach "$BASELINE_ROOT" "$BASE_SHA"
git -C "$RUST_REPO" worktree add --detach "$CANDIDATE_ROOT" "$BASE_SHA"
git -C "$RUST_REPO" diff "$BASE_SHA" "$CACHE_SHA" -- examples/horizon.rs > "$EVIDENCE_DIR/horizon-regex-cache.patch"
git -C "$CANDIDATE_ROOT" apply "$EVIDENCE_DIR/horizon-regex-cache.patch"

CARGO_TARGET_DIR="$BASELINE_ROOT/target" cargo build --release --locked --offline \
  --manifest-path "$BASELINE_ROOT/Cargo.toml" --example horizon
CARGO_TARGET_DIR="$CANDIDATE_ROOT/target" cargo build --release --locked --offline \
  --manifest-path "$CANDIDATE_ROOT/Cargo.toml" --example horizon

taskset -c 0 python3 "$CPU_PROBE" \
  --baseline "$BASELINE_ROOT/target/release/examples/horizon" \
  --candidate "$CANDIDATE_ROOT/target/release/examples/horizon" \
  --candidate-root "$CANDIDATE_ROOT" \
  --candidate-source-sha c18388f257fea77a421c0de4878746b995f130cb532a4e13a7b5227fa41e21cc \
  --rust-root "$BASELINE_ROOT" --ruby "$RUBY" --ruby-root "$RUBY_ROOT" \
  --ruby-gem-home "$RUBY_GEMS" --liquid-root "$LIQUID_ROOT" \
  --theme-root "$HORIZON_ROOT" --fixture "$BASELINE_ROOT/compat/horizon/store.json" \
  --batches 3 --iterations 100 --warmup 50 --worker-cpu 2 --seed 20261001 \
  --output-dir "$EVIDENCE_DIR/cpu"
```

Use Ruby host revision `7db1cc9` and the pinned Liquid/Horizon checkouts above. The output directory must be fresh. CPU assignments are specific to the recorded machine; another host needs distinct physical cores. Preserve all batches and fingerprint the new binaries rather than assuming they reproduce the original bytes or timings.

The original [warm-render diagnostic script](diagnostics/regex_warm_ab.py) and [gzip-compressed instrumentation patch](diagnostics/regex_instrumentation.patch.gz) support the separate regex-constructor attribution. Decompress before applying; the uncompressed patch hash remains recorded in the provenance JSON. Instrumented timings are diagnostic and are excluded from the unprofiled CPU comparison.
