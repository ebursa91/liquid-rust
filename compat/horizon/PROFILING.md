# Diagnose Horizon rendering costs

Build the Rust host with `--features profiling` to collect nested template, tag, filter, selector, lookup/frame and platform-adapter spans. The feature is off by default: ordinary library builds have no tracing dependency, parser wrappers or runtime hooks. Enabling it adds parser wrappers even when no subscriber is active; measure that configuration separately.

The library emits standard `tracing` spans and leaves subscriber choice to the application. The example installs its collector only around the requested CLI operation. It neither changes the global subscriber nor records Liquid values, evaluated paths, argument literals, error messages, store identifiers, credentials or absolute source paths.

## Capture a warm full page

Keep the external Horizon checkout at `5acd1b6b66c02f61d3216e3adace5dd9e0404fc9` and supply our synthetic JSON. Generated HTML/CSS and full diagnostic artifacts remain local.

```bash
cargo build --locked --release --example horizon --features profiling
./target/release/examples/horizon \
  --theme-root /absolute/path/to/pinned-horizon \
  --fixture compat/horizon/store.json --page index --scope page \
  --warmup 50 --iterations 3 \
  --benchmark-json /tmp/horizon-profile/benchmark.json \
  --profile-json /tmp/horizon-profile/templates.json \
  --profile-level templates
```

The collector advertises DEBUG as its maximum level for `templates`, allowing tracing to reject detailed TRACE sites before dynamic filtering. Detailed mode advertises TRACE and retains phase-sensitive filtering. Start with `templates`; repeat with `--profile-level detailed` to expose node/filter/lookup costs. Add `--profile-trace` to retain a bounded Chrome trace timeline in the same JSON. Detailed instrumentation is diagnostic work and is not a production throughput benchmark. Initialization and warmup have separate phases; high-frequency detailed spans are disabled there, reserving collector space for measured rendering.

`aggregates` contains stable site IDs and parent IDs, phase, structural labels, calls, outcome counts and inclusive/exclusive **active wall nanoseconds**. `folded_stacks` uses exclusive active-wall weights, suitable for a wall-time flame graph. `traceEvents` uses Chrome complete events in microseconds. These clocks include instrumentation, scheduling and filtered work. They are not CPU samples or allocation counts. Inclusive rows overlap; summing them double-counts work.

Check `operation_succeeded`, `selected_phases_complete`, per-phase completeness and dropped counters before interpreting costs. The collector bounds 65,536 aggregate sites, 64 nested activations per thread, 16 threads and 32,000 optional timeline events. Initialization/warmup share a separate 512-site allowance. Failed attempts export diagnostics with `operation_succeeded: false` and keep their original nonzero exit/error; errors and arguments are never copied into the profile. A bounded capture can retain partial evidence; an incomplete timeline cannot establish exact attribution. Errors still close synchronous scopes during normal unwinding; release `panic=abort`, forced termination and process exit cannot guarantee a completed export.

## Profile a native gRPC render

The native `horizon-grpc-client` accepts the same `--profile-json PATH`, `--profile-level templates|detailed` and bare `--profile-trace` flags. Point `--renderer` at the profiling-enabled Horizon binary. The client passes the verified authoritative context on stdin and forwards only these diagnostic options. A single native invocation is a cold renderer process; use prepared contexts and excluded warmups for warm-render investigations. Fetch/validation and renderer process timing remain in the separate native report, outside the rendering spans.

Native profile paths may not overlap theme/renderer inputs or HTML/CSS/RPC reports. An existing path can only replace a previous render-profile JSON. Its old profile is cleared before fetch, so failed authentication/fetch cannot leave a stale successful rendering profile. No service token, endpoint, tenant/cart/request ID or context content is added to span fields. Forced child timeout can prevent a completed profile; the native driver still kills and reaps it.

## Locate the expensive Liquid operation

Host spans label only catalog-validated relative templates. Parsed node/filter spans include registered syntax names and `buffer_id`, `line`, `column`, `byte`. Coordinates refer to the parser input buffer. In `{% liquid %}`, plugins create secondary buffers: child coordinates are **not original external-file lines**. The enclosing original node and template label identify the call site. Opaque buffer IDs distinguish generated buffers and are not stable across separate runs.

Lookups expose depth and owned/borrowed, found/missing, delegated and success outcomes without exposing variable names or values. Host spans distinguish snippet arguments, conversion to owned values, prepared scopes/settings, translation, sorted JSON, stylesheets and response assembly. A long parent includes child work; use its exclusive time to investigate work outside enabled child spans.

Parser-created operations have location labels. Programmatically built renderables and anonymous `FilterChain::new` filters receive their enclosing scopes unless their authors add spans. The example collector supports synchronous, well-nested thread activations. Concurrent entry of one span, non-nested parents, overflow and clock/balance errors invalidate exact exclusive attribution. Applications spawning threads must propagate their chosen dispatcher; async applications should instrument futures rather than hold an entered guard across `await`.

## Collect independent CPU samples

Use CPU sampling to distinguish busy code from wall time. The custom `profiling` Cargo profile inherits the existing release flags (fat LTO, one codegen unit, panic abort), adding line tables. Frame pointers improve callchain recovery:

```bash
RUSTFLAGS='-C force-frame-pointers=yes' \
  cargo build --locked --profile profiling --example horizon
```

On Linux with `perf` installed and user-mode sampling permitted, record only your launched renderer. For example, `perf record -g --call-graph fp -e cpu-clock:u -- ./target/profiling/examples/horizon ...` records the CLI operation, including initialization. Separate warm requests before attributing production render costs. Keep sampled addresses and process maps local. System-library frames without debug information remain unresolved rather than being attributed to nearby exported functions.

The owned-worker [CPU capture tool](diagnostics/owned_cpu.py) starts a worker, verifies 50 warmups against the independent homepage golden, attaches the [user-mode sampler](diagnostics/owned_cpu_sampler.c), and hashes every measured response. It requires a frozen build manifest with all source fingerprints, binary digest and explicit frame-pointer/profile flags; sources, binary, sampler, fixture and theme are verified before and after. It does not change kernel settings or attach to another application's process. Its timings are not benchmark results. Resolve captured frames with `symbolize_cpu.py --capture /absolute/path/to/capture`; it validates the executable and requires a Linux PIE build, retains inline renderer frames, and leaves system-library samples unresolved.

## Measure profiling overhead

[profile_overhead.py](diagnostics/profile_overhead.py) runs three randomized serial batches of the same warm page with four configurations: feature off, feature enabled without a collector, template-level collection and detailed collection. Both binaries use identical release flags. At least 50 warmups and 20 measured renders per process are required. Every measured digest and final artifact must match an independent oracle. Initialization, hashing and exports stay outside the internal render timer.

Supply a manifest containing `source` (`head` plus every tracked/unignored file fingerprint), and `variants.feature_off` / `variants.feature_on_inactive`, each with `binary`, `fingerprint`, `profile: "release"`, explicit `features` (`[]` versus `["profiling"]`) and identical recorded build flags. The tool rejects incomplete measured-phase attribution and evidence directories overlapping source/theme inputs. Supply an oracle with `html` and `css` byte counts/SHA256 from the Ruby comparison, then run the tool with `--manifest`, `--rust-root`, `--theme-root`, `--fixture`, `--oracle`, `--output-dir`, explicitly pinned controller/worker CPUs and `--page`. Build and test before timing; stop concurrent build/load generators. Shared-host ranges remain part of the result. The summary retains phase counters, category totals, top measured sites and each full profile fingerprint; complete raw profiles remain beside their individual local benchmark reports.

[profile_worker_cpu.py](diagnostics/profile_worker_cpu.py) repeats those four configurations through the verified stdio worker. Run it with the same manifest/oracle/source/input paths and explicitly pinned controller/worker CPUs. It requires three randomized batches, 50 excluded warmups and 100 measured requests per process. Worker `/proc` user+system CPU starts after READY and ends after the last verified framed response, before EOF and profile export. It includes rendering, instrumentation, framing and pipe writes; excludes startup, warmup and parent hashing. The recorded `SC_CLK_TCK` resolution is averaged over the request window (10 milliseconds on the measured host). CPU/request is distinct from internal render wall time and HTTP RPS. The driver verifies every full response and requires complete `serve` attribution for active collection.

The CPU capture manifest instead uses `source_files`, `binary`, `binary_fingerprint`, `profile: "profiling"`, `features: []` and `build_flags.RUSTFLAGS`. Run `owned_cpu.py --help` for required paths. Keep manifests and output directories outside the source tree so their own creation cannot change a source guard.

## Design references

The approach follows the [Rust Performance Book's sampling/debug-info guidance](https://nnethercote.github.io/perf-book/profiling.html), [tracing's application-owned subscribers and span semantics](https://docs.rs/tracing/0.1.44/tracing/index.html), and [Cargo's profile/LTO definitions](https://doc.rust-lang.org/cargo/reference/profiles.html). Core instrumentation uses only optional `tracing` with `std`; the example collector uses registry/std subscriber support without a global logger, background writer or unbounded event channel.
