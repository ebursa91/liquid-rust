# Benchmark the synthetic Horizon homepage

The runner compares the release Rust renderer with the standalone Ruby renderer
in three modes: plain Ruby, Ruby with YJIT, and Ruby with YJIT inside a Fiber.
Select the Ruby executable explicitly for a versioned run. Ruby 4.0.7 is the main
campaign runtime; Ruby 3.4.10 remains the earlier compatibility baseline. Separate
runs for different versions are separate experiments.

The inputs are unchanged external Shopify Liquid 5.14.0
(`4e39ae4cc3da73921923c0669e0fc84a66b2f696`) and Horizon 4.2.0
(`5acd1b6b66c02f61d3216e3adace5dd9e0404fc9`), plus the same synthetic store JSON.
The public Ruby `fixtures/store.json` must match the supplied fixture byte for
byte. Both external checkouts must be clean. The full fixture validator runs
before measurement.

Keep the output directory outside source checkouts and separate from all input
files, including the fixture, executables and optional comparison manifest.
Input/output collisions are rejected before old success markers are removed.

## Run

Install the selected Ruby's dependencies and build Rust before invoking the
runner. The runner does not install, build, fetch or call network services.
Use a release binary from the source checkout you identify with `--rust-root`:

```bash
cargo build --release --locked --offline --example horizon

python3 compat/horizon/benchmark.py \
  --ruby /absolute/path/to/ruby-4.0.7/bin/ruby \
  --ruby-root /absolute/path/to/horizon-ruby-renderer \
  --ruby-gem-home /absolute/path/to/ruby-4.0-gems \
  --ruby-gem-path /absolute/path/to/ruby-4.0-gems \
  --liquid-root /absolute/path/to/pinned-liquid \
  --theme-root /absolute/path/to/pinned-horizon \
  --rust-root /absolute/path/to/liquid-rust \
  --rust-binary /absolute/path/to/liquid-rust/target/release/examples/horizon \
  --fixture /absolute/path/to/liquid-rust/compat/horizon/store.json \
  --scope page --iterations 30 --warmup 50 --batches 7 \
  --cold-iterations 3 --seed 20261001 \
  --output-dir /tmp/horizon-benchmark
```

Omit the gem arguments when dependencies already reside in the selected Ruby's
normal gem paths. The runner removes inherited `RUBYLIB`, `RUBYOPT`,
`RUBY_YJIT_ENABLE`, `GEM_HOME`, `GEM_PATH` and `BUNDLE_PATH`, then applies the
explicit gem arguments. This prevents an earlier runtime's environment from
selecting incompatible native gems. Plain Ruby receives `--disable-yjit`; the
other Ruby modes receive `--yjit`. Worker reports must confirm the actual runtime
version and YJIT state, plus consistent actual dependency versions and paths
across all Ruby modes/windows. Ruby loads `bundler/setup` inside its own process so the installed Gemfile.lock dependency versions are activated consistently. This loading belongs to cold startup and is outside warm render timers. No separate `bundle exec` launcher is timed.

Use `--expected-comparison /tmp/horizon-comparison/comparison.json` to require
previously established output digests and fixture/pin metadata too. The runner
also creates fresh correctness preflights for every variant before measurement.
A small command check can use `--iterations 2 --warmup 1 --batches 1
--cold-iterations 1`; those counts are insufficient for a performance conclusion.

`--cpu N` optionally applies Linux `taskset` to every worker. The CPU must belong
to the process's permitted affinity set. `--measure-rss` optionally wraps every
worker in Linux GNU `time` to collect peak RSS. Its small launcher cost is then
included in every cold measurement. `--timeout` bounds each process.

## What is timed

| Measurement | Included | Excluded |
| --- | --- | --- |
| Cold, fresh process | Process startup, dependency loading, fixture/source reads, parsing, first render, CLI validation, HTML/CSS/report writes, process exit | Builds, dependency installation, orchestration's correctness checks |
| Warm, reused AST | Each complete render with fresh request runtime/context and reset stylesheet/source/filter diagnostics; rendering and stylesheet collection | Worker startup, fixture loading, excluded warmup renders, HTML/CSS hashing, artifact/report writes |

Cold measures each engine's actual one-shot CLI. Ruby's CLI verifies the Liquid
and theme Git pins inside the process; Rust relies on the orchestrator's external
checks. This validation cost is therefore asymmetric and included in cold
results. Cold figures describe those CLIs, including their provenance checks;
they cannot establish an engine-only startup speedup. Worker-reported
`initialization_ms` and `first_render_ms` remain separate diagnostics: their setup
boundaries differ, so the runner does not rank or combine those fields. Warm render-only figures
provide the closer engine comparison, with the same request output workload.

Each warm process retains parsed AST/source/schema caches but constructs fresh
request state. It never caches the rendered response. The Ruby Fiber variant
creates/resumes a Fiber for the request; it is one serial request at a time,
without an asynchronous scheduler or a concurrency claim. Ruby's
[Fiber documentation](https://docs.ruby-lang.org/en/4.0/Fiber.html) describes this
cooperative execution model.

Cold wall time uses Python’s monotonic nanosecond clock around process creation
and `communicate()` through process exit. Output is drained through a pipe; the
log is written after the timer stops. This avoids the polling delay of
`wait(timeout)`. A timeout terminates the worker process group and preserves the
captured failure log.

Every excluded warmup and measured render is independently rendered and its
HTML/CSS digested outside the rendering timer. Worker sample digests and final
artifact bytes must match the pinned Ruby preflight. A differing sample, missing
required metric, wrong pin/version/JIT state, debug Rust binary, modified input or failed
worker prevents a success report. Unsupported CPU/allocation metrics stay null,
never zero. The runner deletes its previous success marker before starting.

The default 50 warmup requests exercise lazy parsing, GC and JIT compilation
before the 30 measured requests in each process. This is a specified warmup,
not proof of universal steady state. Check longer warmup runs and raw timing
trends before a strong claim. The official
[YJIT documentation](https://docs.ruby-lang.org/en/4.0/jit/yjit_md.html) explains
its compilation threshold, the benefit of repeated execution, and the overhead
of extended statistics. This runner does not enable `--yjit-stats` or tracing.
GC remains enabled; normal GC/JIT work occurring during a render remains timed.

## Read the result

`benchmark.json` contains original numeric timings, process commands and order,
worker diagnostics, input/output digests, runtime/binary metadata, Git heads,
dirty status, source fingerprints, host load and optional RSS. `summary.txt`
contains milliseconds, pooled medians/p95 and individual batch medians. Generated
HTML/CSS, raw reports and logs remain in the external output directory.

Engine order and the cold/warm rounds are shuffled with the recorded fixed seed.
A batch has independent warm processes for all four variants and independent
fresh processes for every cold sample. Measurements run serially. There are
multiple process windows, so a single favorable process does not define the
result. The descriptive 95% percentile bootstrap interval resamples batch
medians, not correlated requests inside one worker. Fewer than three batches
produce no interval. Report p95, the batch median range and host load alongside
medians; do not select only the fastest batch. Paired Ruby/Rust batch ratios are
reported separately for cold and warm phases.

Affinity pins a worker; it does not reserve a CPU, quiet its sibling thread,
control frequency scaling, flush OS caches or suppress other applications.
These are fresh-process cold starts with ordinary filesystem caches, not
cold-disk measurements. Load averages and permitted CPUs are recorded. A busy
shared workstation warrants wider bounds and a separate quiet rerun. These
numbers concern this bounded synthetic homepage, these host adapters and the
recorded versions/configuration; they do not establish universal Liquid or
Shopify performance.

Optional `process_peak_rss_kib` is the maximum resident memory over the worker's
entire lifetime, including initialization, AST caches and JIT code. Ruby's
`samples_allocations` counts allocated Ruby objects per render, not allocated
bytes or peak RSS. No Rust allocation count is inferred. Missing metrics are
explicitly unavailable. Binary SHA256 and source Git metadata are recorded
independently: `--rust-root` is the operator's source binding, not proof that an
arbitrary supplied binary was built from that commit. Build and validate the
binary from the recorded source before the run; no compilation is timed.
