# Horizon deep performance investigation, 2026-10-01

Repeated copies of section and `closest` trees were a confirmed cost in this mock host. Immutable scope and closest snapshots reduced requested new-allocation bytes on the 100-product collection page by **94.5%**, from **3,169,407,935** to **174,139,355 bytes per warm request**. The same page's render-wall batch medians fell from 2,307–2,969 ms to 84–104 ms. These measurements concern the pinned synthetic Horizon workload and this adapter, not a general ranking of Liquid engines.

The default release implementation is retained. ThinLTO with one codegen unit was slower than the source-identical default on all three collection batches and did not consistently improve homepage CPU.

## What changed

`ScopeOverlay` holds an immutable parent `Arc` and the child's own platform overrides. `ClosestView` holds the fixture context, an earlier scope or an earlier closest snapshot, plus explicitly supplied overrides. Empty closest overrides reuse the prior snapshot; siblings clone an `Arc` instead of the product/collection tree. Parent edges point to earlier snapshots, and the context owns no reverse scope reference, so this ownership graph is acyclic.

Lookup uses key presence: explicit nil still shadows a parent. Settings materialization and section collection overrides retain their order. Assignments, render kwargs and content_for locals stay in fresh request frames and do not enter the immutable platform snapshots. Typed properties, logical sorted ObjectView iteration, explicit materialization, sandbox isolation and sibling isolation remain covered by focused tests. Source/schema/AST caches persist; responses are freshly rendered.

The frozen candidate passed 43 host tests and Clippy. The committed default change also completed the genuine offline matrix: 48 cases and 192 exact Ruby/Rust renders. The final native matrix also passes 112 scopes / 236 renders across two tenants. The separate calibrated HTTP results are recorded in [RPS_RESULTS.md](RPS_RESULTS.md).

## Requested allocation traffic

The following are successful **new allocation requests in bytes**, not live, retained, resident or peak memory. Reallocation new sizes are separate.

| Full page | a468 baseline | Scope overlay | Overlay + closest sharing |
| --- | ---: | ---: | ---: |
| Original four-product index | 74,673,525 | 58,110,120 | 46,073,306 |
| 100-product index | 794,053,477 | 427,356,128 | 184,953,354 |
| 100-product product | 104,146,758 | 93,876,145 | 79,524,889 |
| 100-product collection | 3,169,407,935 | 1,841,926,735 | 174,139,355 |
| 100-product collection page two | 3,169,499,240 | 1,842,008,216 | 174,203,644 |

Collection allocation calls fell from **10,091,921 to 609,104**. Its separately counted reallocation traffic remained **9,005,912 new-size bytes in 13,371 calls**. The original index separately requested 4,849,794 reallocation new-size bytes in every version. Deallocation can free pre-request/cache data; subtracting it does not produce a live-memory measurement. No Ruby allocation-byte or memory comparison is implied.

The identical temporary System allocator wrappers counted each final warm full-page render before hashing, reports or artifact writes. Fifteen processes executed 45 fresh renders with one excluded warmup and two measured results per process. All measured digests and final raw artifacts matched independent goldens; source and binary fingerprints remained fixed. Atomic-counter overhead is present. These diagnostic runs support allocation attribution, not timing claims.

## Original homepage worker CPU

Milliseconds of worker user + system CPU per request, averaged over 100 requests in each process:

| Variant | Batch 1 | Batch 2 | Batch 3 | Median of batch averages |
| --- | ---: | ---: | ---: | ---: |
| a468 default | 36.4 | 37.7 | 33.2 | 36.4 |
| Scope overlay default | 27.7 | 27.0 | 25.7 | 27.0 |
| Overlay + closest default | 26.7 | 30.5 | 27.2 | 27.2 |
| Same closest source, ThinLTO + one codegen unit | 23.5 | 36.0 | 29.9 | 29.9 |
| Ruby 4.0.7 YJIT | 31.3 | 31.5 | 24.3 | 31.3 |

Closest sharing showed no clear additional homepage CPU benefit over the overlay. Relative Ruby ranking changed by batch. The Linux `/proc/pid/stat` delta runs after READY and 50 warmups through the final verified pipe response. It includes rendering, JSON framing and pipe writes, excludes startup/warmup and parent verification CPU, and has 10 ms ticks averaged over 100 requests. It is neither render-only latency nor HTTP RPS.

## Genuine 100-product page render wall

Internal render-wall milliseconds; each cell gives the three process-batch medians, followed by the pooled median in parentheses. Displayed values are rounded to three decimals; raw samples retain full precision.

| Variant | Product | Collection |
| --- | --- | --- |
| a468 default | 48.121 / 156.808 / 77.387 (90.911) | 2,968.912 / 2,306.983 / 2,345.116 (2,482.425) |
| Overlay + closest default | 93.324 / 44.278 / 45.576 (47.783) | 104.005 / 83.561 / 90.917 (87.342) |
| Same closest source, ThinLTO + one codegen unit | 115.072 / 36.268 / 100.798 (84.496) | 199.194 / 115.301 / 273.536 (197.710) |
| Ruby 4.0.7 YJIT | 89.749 / 89.235 / 118.141 (95.316) | 125.617 / 171.144 / 148.079 (146.330) |

The collection improvement appeared in every paired batch: baseline/default ratios were **28.546, 27.608 and 25.794**. Product distributions varied substantially; the candidate was slower than baseline in the first product batch. ThinLTO's collection batch medians were 1.92×, 1.38× and 3.01× the default candidate's. This supports keeping the default build, without claiming compiler flags caused a universal slowdown.

Three seeded randomized serial batches ran four variants × two pages, each with 50 excluded warmups and 20 measured requests: **480 measured results, 1,200 warmups and 24 final HTML/CSS artifact pairs**. Rust `Instant` and Ruby `CLOCK_MONOTONIC` surround fresh request/runtime construction, diagnostic reset, unchanged template execution and full HTML/CSS assembly. Hashing, writes, initialization, dependency loading and warmup are outside those samples. Every measured digest and final artifact matched the independently recorded offline oracle. Default/ThinLTO source bytes were identical; the effective compiler change was fat LTO to ThinLTO. The default already uses one codegen unit; the candidate explicitly retained it.

## Evidence and deferred work

The earlier a468 user-mode profile placed `Object::clone` in 45.09% of inclusive sampled callchains. The profile collected 25,576 samples with no reported loss over 600 measured homepage requests. KString clone/drop self samples were 9.235%/7.796%, and Value clone self samples were 3.421%. GlobalFrame lookup appeared in 12.125% of inclusive callchains. Inclusive rows overlap; 41.191% of self samples were unresolved libc code and cannot be assigned to a particular allocator operation. This is a baseline diagnostic build with frame pointers/debug information, not a profile of the final optimized host.

Render keyword costs exist both during mutable-frame evaluation and during subsequent borrowed-to-owned conversion. Mutable-frame results release RefCell guards by owning selected values; returning ordinary borrows would require a different storage/lifetime design. The isolated borrowed-keyword candidate, loop-local view and Path-capacity experiments showed no consistent CPU gain and remain deferred. JSON's sorted protocol currently materializes an owned Liquid value and a second JSON tree; existing profiling does not establish it as dominant. Prepared schema/settings copies, pagination substitution and small CSS/diagnostic allocations likewise remain targeted profiling candidates rather than speculative fixes. Focal-point registry construction occurs during initialization, outside warm request measurements.

All comparisons were serial on a shared Linux i9-9900K host with worker CPU 2 and controller CPU 0. The larger-page experiment observed one-minute load of 13.10–30.23. Rust/Cargo was 1.98.1 and Ruby was 4.0.7 with actual YJIT enabled; both used the locked dependencies and unchanged external pins. Affinity does not reserve cores or control frequency. Batch ranges and raw samples are retained; a pooled median hides scheduling variation. These are offline synthetic full pages, not gRPC fetch or HTTP service throughput. Do not invert render medians into RPS or infer universal engine speed/memory rankings.

Numeric evidence comes from the final CPU report, minimal allocation comparison, baseline profile summary and larger-page warm comparison. Production baseline is `a468a7ac18155f98fb9fe51881eafac50bae8105`; final candidate host SHA256 is `9854a7402ec298a70c0389df9f25fb2f534d871322231ee7078aed1881906504`. Horizon is pinned to `5acd1b6b66c02f61d3216e3adace5dd9e0404fc9`, Liquid Ruby to `4e39ae4cc3da73921923c0669e0fc84a66b2f696`. No theme source, fixture contents or generated HTML/CSS belong in the public performance evidence.


## Numeric evidence and source identity

- [All seven isolated CPU candidates](results/2026-10-01-deep-cpu-candidates.json): 1,050 warmups and 2,100 measured renders. Scope sharing improved CPU in all three paired batches; Path capacity/inline storage, loop locals and borrowed render keywords did not consistently improve it.
- [Final five-variant CPU comparison](results/2026-10-01-deep-cpu-final.json): 750 warmups and 1,500 measured renders, including source-identical default/ThinLTO builds.
- [All larger-page wall samples and paired ratios](results/2026-10-01-deep-large-pages.json).
- [Requested allocation counters](results/2026-10-01-deep-allocations.json): 45 instrumented renders. Full source archives/patches were independently audited at build; this diagnostic driver guards the host file and executable before/after rather than every tracked runtime file.
- [Baseline CPU profile function counts and build provenance](results/2026-10-01-deep-cpu-profile.json). Maps, addresses and callchains remain local. Its capture driver did not enforce the full before/after tree guards used by the unprofiled comparisons; independently audited frozen build/source metadata supports this diagnostic.
- [Final offline parity](results/2026-10-01-deep-offline-parity.json) and [native gRPC parity](results/2026-10-01-deep-grpc-parity.json).

Shipped commits: [shared scopes, 3ba954f](https://github.com/ebursa91/liquid-rust/commit/3ba954f023c9056a2efef51d09697e3ede34021c) and [shared closest, ecd9ee4](https://github.com/ebursa91/liquid-rust/commit/ecd9ee44212d95ac70dec90fa21b4aa0c99b4d6c). The measured default candidate's binary SHA256 is `4fd5a38a39843929a37c42c2a70fd047e899c337440b2744a936d0815156fd6f`; ThinLTO+CGU1 is `327a11383281d012e45253147895afce8cbc568d38f895d7e6d768dff0fa3d72`. All 263 tracked files in each frozen a468-based source archive were checked around the unprofiled comparisons. Both candidates initially existed as frozen archives based on a468; the exact default host source subsequently became ecd9ee4. Publication commits do not change that implementation.

The earlier [regex investigation](PERFORMANCE_INVESTIGATION.md) remains a separate checkpoint. Its CPU baseline, source state and machine load differ; do not combine separately executed medians into a precise cumulative speedup.

Reproduce with [the original diagnostic tools and setup](DEEP_REPRODUCTION.md). Every measured response and final artifact must match pinned independent goldens. Keep full batch distributions and failed candidates, and report each new build's actual fingerprints.
