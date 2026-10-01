# Synthetic Horizon homepage benchmark, 2026-10-01

Ruby 4.0.7 with YJIT had the lowest observed warm median in this run: **23.75 ms**, compared with **65.66 ms** for the Rust release host. All 2,328 renders, including warmup, fresh-process correctness checks and measured requests, matched the independent original Ruby oracle's exact HTML/CSS. These numbers compare the two configured full-page hosts, including their synthetic Shopify adapters. They are not universal Liquid engine or Shopify throughput results.

The [numeric-only data](results/2026-10-01-linux.json) contains every measured process window, per-render samples, runtime/input fingerprints, load and summary statistics. External theme sources, generated HTML/CSS, logs and private paths are omitted. The complete local reports remain separate from both source repositories.

## Workload and method

- One entirely synthetic Shopify-shaped store: four products, seven variants, three collections, two cart lines/three units. Shared fixture: 772031 bytes, SHA256 `867c41e0929881290af2b261af64146632bf0287524f5a6c55714af32e819f98`.
- Unmodified external Horizon 4.2.0, `5acd1b6b66c02f61d3216e3adace5dd9e0404fc9`; Ruby Liquid 5.14.0, `4e39ae4cc3da73921923c0669e0fc84a66b2f696`.
- Measured implementation revisions: Ruby `aaeccd77bef7cf528ed945e30b309e1471369265`; Rust `2482203c7aa5b530bb2ebf175dab06d90aef6512`. Both were committed, clean and unchanged throughout measurement. The later metrics/docs commits do not change their implementation.
- Ruby 4.0.7, actual YJIT enabled only for the two named modes. Bundler activates the locked dependencies in every Ruby process: BigDecimal 4.1.3, strscan 3.1.8, prism 1.9.0; bundled JSON 2.18.0. Rust/Cargo 1.98.1, x86_64 Linux release build.
- Intel Core i9-9900K, 16 logical CPUs; workers pinned to CPU 2 without reserving it or its sibling. The shared workstation's one-minute load ranged **9.24–18.96**, median 12.83. No frequency or filesystem-cache controls were applied.
- Seven batches, randomized serial engine/phase order, seed 20261001. Each warm process excluded 50 independently checked requests before 30 measured renders: **210 measured renders per variant**. Each variant also had **21 fresh-process cold measurements**. Builds/tests/installations finished before measurement.
- Fresh request globals, contexts, Drops/runtime state and diagnostics on every request; retained source/schema/AST caches, enabled GC, no rendered-response cache. Warm timers include complete rendering and HTML/CSS assembly, excluding hashing and artifact/report writes.

## Warm complete render

All values are milliseconds. Median and p95 pool the 210 samples. Range and descriptive bootstrap interval use the seven independent batch medians; the interval describes the median of those batch medians, not the pooled sample median or a controlled-machine confidence bound.

| Variant | Median | p95 | Batch median range | Batch-median descriptive 95% interval |
| --- | ---: | ---: | ---: | ---: |
| Rust release | 65.66 | 259.73 | 62.51–198.27 | 63.29–104.10 |
| Ruby 4.0.7, plain | 55.42 | 125.15 | 52.27–102.51 | 53.30–71.25 |
| Ruby 4.0.7, YJIT | 23.75 | 35.19 | 22.01–30.67 | 22.90–27.54 |
| Ruby 4.0.7, YJIT + Fiber | 24.37 | 42.98 | 22.64–38.84 | 22.85–28.43 |

The paired Ruby-YJIT/Rust batch-median wall-time ratio was **0.353**, descriptive 95% bootstrap interval **0.226–0.391**. Plain Ruby/Rust was 0.834, interval 0.537–1.140; this run does not establish a consistent plain-Ruby advantage. Load and scheduler variation are visible, especially in Rust's sixth batch, and warrant a quiet-machine rerun before stronger decisions.

Direct YJIT and Fiber YJIT were close. This run creates/resumes one Fiber per serial request on one Ruby thread, with no asynchronous scheduler or CPU parallelism. It does not establish a performance benefit from fibers or concurrent-request throughput. Cooperative request isolation is tested separately. The specified 50-request warmup is not proof of universal JIT steady state.

## Cold actual CLI

Cold includes process startup, dependency loading, fixture/source reads, initial parsing/rendering, validation, HTML/CSS/report writes and exit, with ordinary filesystem caches. Ruby additionally activates Bundler's locked dependencies and performs Git provenance checks inside its CLI; Rust's source provenance is checked by the parent. These figures compare the actual CLIs with that asymmetry, not equal-work engine startup.

| Variant | Median ms | p95 ms |
| --- | ---: | ---: |
| Rust release | 157.20 | 249.94 |
| Ruby 4.0.7, plain | 385.76 | 647.35 |
| Ruby 4.0.7, YJIT | 593.68 | 823.75 |
| Ruby 4.0.7, YJIT + Fiber | 617.64 | 873.99 |

## Ruby CPU and allocation observations

| Ruby mode | Median process CPU ms/render | Median allocated Ruby objects/render | Median worker VmHWM KiB |
| --- | ---: | ---: | ---: |
| Plain | 55.22 | 58783 | 55768 |
| YJIT | 23.68 | 59236 | 65088 |
| YJIT + Fiber | 24.29 | 59245.5 | 66392 |

Allocations count Ruby objects, not bytes. VmHWM covers the Ruby worker's lifetime up to report generation, including caches/JIT/warmup, not memory retained by a single request. GNU time was unavailable; Rust CPU/allocation/RSS metrics are unavailable. No cross-engine memory ranking is supported.

Both hosts were optimized before this comparison. Ruby retains immutable source/JSON/schema/AST caches and isolates mutable request/template wrappers. Rust retains ASTs and borrows immutable store globals with small request overlays instead of repeatedly copying the full store. Diagnostic profiles justified those changes; noisy earlier diagnostic timings are not reported as controlled before/after speedups.

## Correctness and next measurements

Expected output: HTML 435304 bytes, SHA256 `d97c35b3ba08f026536cb4c469623acb9957af59fb9a9171db012958515fe990`; collected CSS 279112 bytes, SHA256 `67a6538e0b763c32ced001728ebf68f375dec497d4fb91c0ee136658ff9f2034`. No normalization, Ruby output substitution or response caching is used. The independent original Ruby host also matches the new Rust implementation in two separate repeat processes.

Continue with a quiet shared-version rerun and longer-warmup sensitivity, then larger original mock stores, product/collection pages, additional locales/configurations and realistic request concurrency. Keep exact output parity as the gate for every optimization; profile remaining Rust costs before selecting another change. Treat new theme pins as separate experiments and retain their raw timing distributions.

Reproduce using [the benchmark method and runner](BENCHMARKING.md) and the [public Ruby renderer](https://github.com/ebursa91/horizon-ruby-renderer). See [the rendering and performance goal](ROADMAP.md).
