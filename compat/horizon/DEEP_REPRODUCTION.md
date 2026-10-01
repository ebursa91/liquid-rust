# Reproduce the deep performance measurements

Use separate frozen Rust source directories and an evidence directory outside all source trees. Keep the external Horizon/Liquid checkouts local at the pins in [DEEP_PERFORMANCE.md](DEEP_PERFORMANCE.md). Use Rust/Cargo 1.98.1, Ruby 4.0.7 with YJIT and the Ruby renderer's locked dependencies. Install/fetch/build before timing, then run each experiment serially without tests, builds or other load generators.

The original argument-driven tools are [deep_worker_cpu.py](diagnostics/deep_worker_cpu.py) and [large_pages.py](diagnostics/large_pages.py). Their measured script fingerprints are in the numeric reports. They verify outputs and frozen source/binary inputs; a rebuilt executable needs its own fingerprint. CPU numbers, internal render-wall times and HTTP RPS have different boundaries.

## Prepare frozen candidates

Start every candidate from a complete archive of `a468a7ac18155f98fb9fe51881eafac50bae8105`, including all 263 tracked files. Do not archive a later documentation revision: the diagnostic source guard compares every file against a468. Use `git archive` from the fork after fetching its campaign branch. Apply the gzip-compressed patches with `gzip -dc`, then `git apply` from the appropriate archive directory:

| Role | Patch relative to a468 | Required host source SHA256 |
| --- | --- | --- |
| Baseline | None | `c18388f257fea77a421c0de4878746b995f130cb532a4e13a7b5227fa41e21cc` |
| Shared scopes | [horizon-host-overlay.patch.gz](diagnostics/horizon-host-overlay.patch.gz) | `551336874e0da4d62b01dd93f522cc55310cf5355b9ebfecfe2905ba7ca0d347` |
| Shared scopes + closest | [horizon-host-closest.patch.gz](diagnostics/horizon-host-closest.patch.gz) | `9854a7402ec298a70c0389df9f25fb2f534d871322231ee7078aed1881906504` |
| Shared scopes + closest, ThinLTO | Same closest patch in a separate directory | Same source as default closest |

Build each archive with `cargo build --release --locked --offline --example horizon`, using separate target directories. The three default variants have `RUSTFLAGS`, `CARGO_PROFILE_RELEASE_DEBUG`, `CARGO_PROFILE_RELEASE_LTO` and `CARGO_PROFILE_RELEASE_CODEGEN_UNITS` unset. The optional source-identical compiler experiment sets **both** `CARGO_PROFILE_RELEASE_LTO=thin` and `CARGO_PROFILE_RELEASE_CODEGEN_UNITS=1`, with RUSTFLAGS/debug unset. Record the actual environment, compiler, build command, Cargo.lock digest and pre/post-build source fingerprints; do not label it as an LTO-only change.

The [final CPU report](results/2026-10-01-deep-cpu-final.json) records the full source manifest, patch digests, binary fingerprints and flags for all four Rust variants. Use [the original final manifest](diagnostics/deep-cpu-final-manifest.json) as a template. Create a local manifest with this shape, expanding role placeholders to local absolute paths and updating rebuilt binary fingerprints:

```json
{
  "base_head": "a468a7ac18155f98fb9fe51881eafac50bae8105",
  "variants": [
    {
      "name": "rust_baseline",
      "source_root": "BASELINE_SOURCE",
      "binary": "BASELINE_BINARY",
      "binary_sha256": "SHA256_OF_REBUILT_BINARY",
      "changed_source_sha256": {},
      "build_flags": {
        "CARGO_PROFILE_RELEASE_LTO": null,
        "CARGO_PROFILE_RELEASE_CODEGEN_UNITS": null,
        "RUSTFLAGS": null,
        "CARGO_PROFILE_RELEASE_DEBUG": null
      }
    }
  ]
}
```

Add `rust_host_overlay`, `rust_host_closest` and `rust_host_closest_thinlto` entries. Each declares its exact `examples/horizon.rs` digest in `changed_source_sha256`, the decompressed `patch` path and `patch_sha256`, its executable and all four build overrides. Only the ThinLTO entry changes the two specified flags and supplies `build_provenance` pointing to a local JSON report. The CPU runner additionally creates the Ruby YJIT variant itself.

For the larger-page guard, the ThinLTO provenance report must bind `base_head`, `changed_source_sha256`, `build_flags`, `tracked_file_count`, `cargo_lock_sha256`, the patch digest and binary bytes/digest. It records successful build/source checks with `build_success`, `source_verified_after_build`, `all_tracked_files_verified` true and `production_instrumentation` false. If `source_manifest_path` is provided, it points to an array of `{path, bytes, sha256}` for every tracked file; `source_manifest_sha256` hashes that array's canonical JSON (`sort_keys=True, separators=(",", ":")`), rather than the pretty-printed file. Build/source manifests bind the supplied inputs; they are not signed build attestations.

## Original homepage worker CPU

Run the original four-product fixture with the final manifest above. `$RUST_ROOT` is a Git checkout containing a468 and the current compatibility harness; frozen source archives are declared separately in the manifest. `$RUBY_ROOT` must retain the measured engine code at `7db1cc962c1a90a40613da98fdff18f061680bb8`; subsequent documentation-only revisions are accepted by the guard. `$RUBY_GEMS` is the directory containing the locked installed gems.

```bash
taskset -c 0 python3 "$DIAGNOSTICS/deep_worker_cpu.py" \
  --manifest "$EVIDENCE_DIR/manifest.json" --rust-root "$RUST_ROOT" \
  --ruby "$RUBY" --ruby-root "$RUBY_ROOT" --ruby-gem-home "$RUBY_GEMS" \
  --liquid-root "$LIQUID_ROOT" --theme-root "$HORIZON_ROOT" \
  --fixture "$RUST_ROOT/compat/horizon/store.json" \
  --batches 3 --warmup 50 --iterations 100 --seed 20261003 \
  --worker-cpu 2 --controller-cpu 0 --output-dir "$EVIDENCE_DIR/cpu"
```

The output directory must be new. `/proc` user/system CPU is sampled after READY and warmup through the final verified pipe response. It includes rendering/framing/pipe writes and excludes startup/warmup/parent verification CPU. Preserve all batches; do not invert CPU averages into RPS. The [first seven-candidate manifest](diagnostics/deep-cpu-candidates-manifest.json) used seed 20261002 and the independently frozen experimental patches, retained in the corresponding numeric report and diagnostics directory.

## Genuine 100-product product and collection

First generate the deterministic scenarios with the public Ruby [scenario generator](https://github.com/ebursa91/horizon-ruby-renderer/blob/main/script/generate_scenarios.rb) and its pinned `fixtures/scenarios.json` specification. Use `"$RUBY" "$RUBY_ROOT/script/generate_scenarios.rb" --base "$RUBY_ROOT/fixtures/store.json" --output-dir "$SCENARIOS"` with a fresh output directory. The [offline comparator](https://github.com/ebursa91/liquid-rust/blob/codex/horizon-mock-store/compat/horizon/compare_scenarios.py) invokes this generator twice and verifies byte-identical regeneration. `harbor-100-en-default.json` must match the exact fixture bytes/digest in [the larger-page report](results/2026-10-01-deep-large-pages.json). Run the offline comparator to independently reproduce the goldens before timing, or use the published numeric offline parity report as the hash oracle. External templates and generated bodies remain local.

```bash
taskset -c 0 python3 "$DIAGNOSTICS/large_pages.py" \
  --manifest "$EVIDENCE_DIR/manifest.json" --rust-root "$RUST_ROOT" \
  --ruby "$RUBY" --ruby-root "$RUBY_ROOT" --ruby-gem-home "$RUBY_GEMS" \
  --liquid-root "$LIQUID_ROOT" --theme-root "$HORIZON_ROOT" \
  --fixture "$SCENARIOS/harbor-100-en-default.json" \
  --oracle-report "$OFFLINE_PARITY_REPORT" \
  --batches 3 --warmup 50 --iterations 20 --seed 20261004 \
  --worker-cpu 2 --controller-cpu 0 --output-dir "$EVIDENCE_DIR/large"
```

This runs 24 fresh processes and 1,680 exact renders. The slow baseline still receives all 50 warmups and 20 measured samples; do not reduce its sample count selectively. The internal timers cover fresh request/runtime creation and full HTML/CSS assembly, excluding hashing/writes/startup. Retain every result, batch median, paired ratio, source guard and failure. CPU assignments are specific to the recorded machine; another host needs distinct physical cores.

## Supporting diagnostics

The three allocator patches in `diagnostics/horizon-host-alloc-*.patch.gz` are relative to their corresponding a468/overlay/closest production snapshots. They add the same temporary System allocator forwarding wrapper and counters. Build them separately, enable `HORIZON_PROFILE_ALLOCATIONS=1`, and run the original index plus generated index/product/collection/page-two cases with one warmup and two measured renders. Verify every measured digest/final body against the independent oracle. Counters snapshot the last full render before hashing/writes. Their atomic overhead disqualifies these binaries from speed comparisons. Count new allocation requests and reallocation new sizes separately; deallocations may free data allocated earlier. Do not derive live memory from these totals.

[owned_cpu_sampler.c](diagnostics/owned_cpu_sampler.c) is the original owned-process, user-mode sampling tool. Compile with `cc -O2 -Wall -Wextra -Werror`, and consult its CLI usage. The recorded profile used an a468 baseline build with `RUSTFLAGS="-C force-frame-pointers=yes" CARGO_PROFILE_RELEASE_DEBUG=1`, after 50 warmups and during 600 exact renders. Preserve lost-event counts and map unresolved system code honestly. Inclusive function counts overlap. The recorded driver lacked the timed comparisons' full before/after tree guard; profile results support attribution only, not final-host performance or benchmark speed.

Use [RPS.md](https://github.com/ebursa91/liquid-rust/blob/codex/horizon-mock-store/compat/horizon/RPS.md) for the separate common-HTTP benchmark. Live gRPC fetch/validation throughput and calibrated multi-worker scaling are additional workloads; the prepared rendering experiments do not measure them.
