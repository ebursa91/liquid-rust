# Rust render profiling checkpoint, 2026-10-02

Rust now has opt-in template and detailed Liquid instrumentation, plus independent user-mode CPU capture. Default library builds have no tracing dependency, and the host runtime wrappers/hooks compile out when the feature is disabled. Cargo may still build the example/test subscriber dev dependency. The final profiler and pagination implementation is `13205269362850a9addbab950b37a6bdf5b54e5e`; the first captured profiler is `390f9d4cccca9aa1db03eb3046c49b53922a21fd`. Both inherit the existing fat-LTO, one-codegen-unit, panic-abort release settings. No production profile flags changed.

## Diagnose exact rendering sites

The library emits standard application-owned `tracing` spans. The host exports bounded aggregates, inclusive/exclusive active wall time, ownership/outcome counts, parser-buffer coordinates, optional Chrome timeline events and folded active-wall stacks. Template labels come from the verified catalog; arguments, values, runtime lookup names, credentials, error text and absolute paths are excluded. Generated `{% liquid %}` buffers have their own coordinates; enclosing original nodes/templates identify the external call site.

[Four detailed captures](results/2026-10-02-profiling-captures.json) cover the original homepage, genuine 100-product product/collection pages and a Polish wide-settings page. Each selected measured phase is complete and matches the earlier independent HTML/CSS golden. The separately bounded initialization/warmup allowance drops auxiliary sites, so whole-capture completeness is false. Check selected-phase completeness and dropped/balance counters before attribution. These W1/N1 captures are functional diagnostics.

[Final native profiling validation](results/2026-10-02-profiling-native-validation.json) covers four additional authority-backed cases across both tenants, sizes, three locales, settings profiles and collection pagination. Cold single-render profiles are complete; failed authentication clears the prior profile and reports. Both native clients still pass the full [112-scope/236-render matrix](results/2026-10-02-profiling-grpc-parity.json); feature-off Rust passes [48 offline cases/192 renders](results/2026-10-02-profiling-offline-parity.json). Every earlier context/request/cart/page/output golden remains exact.

## Remaining CPU costs and applied change

The independent feature-off diagnostic executable recorded **27,764 user-mode CPU samples**, zero lost samples and zero unmapped sampled instruction pointers, over 600 verified original-homepage requests after 50 excluded warmups. It uses line tables and frame pointers; sample attribution is separate from normal release performance measurements. [Numeric CPU and ownership evidence](results/2026-10-02-profiling-cpu-hotspots.json) binds the capture to the first profiler commit and binary.

Concrete Object materialization appears in **31.88%** of inclusive sampled callchains. Selecting the nearest owning caller partitions that subset:

| Object-copy caller | Samples | Share of all CPU samples |
| --- | ---: | ---: |
| Assigned-value lookup in `GlobalFrame::try_get` | 3,274 | 11.79% |
| Assignment source converted to owned | 2,401 | 8.65% |
| Collection/product/link-list picker materialization | 1,622 | 5.84% |
| Render/content-for keyword ownership | 1,018 | 3.67% |
| Default-filter selected result | 358 | 1.29% |
| Pagination parent copy | 177 | 0.64% |
| Other snippet ownership boundary | 1 | <0.01% |

General inclusive symbol rows overlap. `FixtureRuntime::try_get` includes assigned-value deep copies; its 19.57% inclusive share cannot be described as dispatch overhead. Boxed-string cloning accounts for 13.01% self samples. The 38.78% self samples in unresolved libc code remain unnamed; allocator attribution needs system debug symbols.

The host now skips cloning each pagination subtree that its replacement immediately discards, preserving key insertion sequence, sibling values and error behavior. A nested regression forbids materialization of the replaced branch/leaf, and genuine-page parity passes. This is a confirmed eliminated copy; these experiments establish no isolated wall-time, CPU or RPS speedup for that small correction. Returning references through mutable assigned-value `RefCell` guards would change the lifetime contract, so those larger ownership boundaries need a separate safe design and measured candidate.

## Profiler overhead

Three seeded randomized serial batches use two frozen release binaries with identical flags: feature off and feature enabled. Four configurations run in fresh processes. CPU windows exclude startup and 50 warmups, then verify 100 full framed responses before EOF/export; **1,800 total requests** including warmup. Wall windows exclude 50 warmups and measure 20 fresh complete renders; **840 total renders** including warmup. All outputs match the independent original Ruby oracle; every active selected phase is complete. Worker CPU includes render/callback/framing/pipe writes and excludes parent hashing. Internal render wall excludes hashing, writes and export. The recorded tick resolution is 10 ms, averaged over each CPU request window.

| Configuration | Median worker CPU ms/request | Batch CPU range | Paired CPU ratio to feature off | Median internal render wall ms |
| --- | ---: | ---: | ---: | ---: |
| Feature off | 25.30 | 22.00–29.70 | 1.000–1.000× | 20.50 |
| Feature enabled, inactive | 32.20 | 32.00–34.80 | 1.077–1.582× | 37.85 |
| Template collector | 27.10 | 26.60–37.60 | 0.912–1.709× | 26.23 |
| Detailed collector | 132.70 | 127.40–134.00 | 4.290–6.091× | 116.87 |

[Final CPU data](results/2026-10-02-profiling-final-cpu.json) and [final wall data](results/2026-10-02-profiling-final-wall.json) retain every batch, sample/counter, load and source/binary fingerprint. Enabled-but-inactive CPU is above feature off in all three pairs; compiling the feature changes wrappers and code generation even without collection. Template collection ratios cross 1.0 on this shared host, which supports no performance benefit from collecting spans. Detailed tracing costs 4.29–6.09 times feature-off worker CPU in these pairs and is for diagnosis. Affinity pins CPUs without reserving their siblings or fixing frequency.

The first collector's [CPU data](results/2026-10-02-profiling-baseline-cpu.json) and [wall data](results/2026-10-02-profiling-baseline-wall.json) remain a separate historical experiment. Its template CPU ratios were 1.59–2.39× and detailed ratios 4.41–6.26×. The shipped collector advertises DEBUG for template mode and TRACE for detailed mode, allowing tracing to reject unused detailed sites before dynamic filtering while preserving phase-sensitive detailed capture. Different shared-host runs establish no precise before/after speedup for this filter change. Active-wall span values include instrumentation and scheduling, so they cannot rank Ruby/Rust speed or yield inverse-latency RPS.

The earlier shared-context optimization and calibrated HTTP comparison remain documented in [the deep investigation](DEEP_PERFORMANCE.md). Those RPS results bind to `ecd9ee44212d95ac70dec90fa21b4aa0c99b4d6c`; no new RPS ranking was measured with this profiler revision.

## Verification and reproduction

Required local checks pass with default/all/no-default features, Clippy, formatting and private-item Rustdoc. Host tests: 45 default and 64 all-features; core: 106; profiling semantics: six integration tests; conformance: 511 passed and one existing ignored. The isolated native client tests/Clippy pass. Independent source, privacy, method, numeric and full-golden audits pass. Rust fork remote CI remains unverified because it has no registered Actions/check results; Ruby and mock-service CI run their version matrices.

Follow the [profiling guide](https://github.com/ebursa91/liquid-rust/blob/codex/horizon-mock-store/compat/horizon/PROFILING.md) for warm template/detailed captures, native gRPC flags, loss-free owned CPU sampling, symbolization and both overhead drivers. Full profiles, sampled addresses/maps, raw logs and generated bodies remain local. Public JSON retains bounded numeric/structural summaries and fingerprints; matching copies are committed in both public repositories. Core MSRV stays 1.83; actual feature-on/off library probes pass at 1.83. The baseline edition-2024 development dependency still prevents full workspace MSRV verification; the isolated Tonic client requires 1.88+.
