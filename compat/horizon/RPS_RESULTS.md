# Final shared-context HTTP throughput, 2026-10-01

The default release Rust host with shared scopes/closest measured **35.75 verified RPS** at four closed-loop clients. Ruby 4.0.7 YJIT measured **33.60 RPS**. All hosts freshly rendered the same original prepared four-product Horizon homepage into identical bytes. [Complete numeric samples and accounting](results/2026-10-01-deep-rps.json) retain every window, latency sample, source/binary fingerprint and failure. Implementation: Rust `ecd9ee44212d95ac70dec90fa21b4aa0c99b4d6c`; unchanged Ruby engine `7db1cc962c1a90a40613da98fdff18f061680bb8`.

RPS counts independently verified successful HTTP responses completed inside a nominal 20-second window, divided by 20. Tables report medians of three fresh-pool batch rates, with all batch ranges retained. Closed-loop p95 pools request latencies across the corresponding three windows, including drain. Shared-host scheduling is visible; these results do not rank languages generally or establish a Fiber advantage.

| Host | One client RPS | Four clients RPS | Four-client batch range | Four-client p95 ms |
| --- | ---: | ---: | ---: | ---: |
| Rust default release | 32.00 | 35.75 | 22.40–40.45 | 229.18 |
| Ruby plain | 14.30 | 16.90 | 15.35–16.95 | 348.70 |
| Ruby YJIT | 26.25 | 33.60 | 26.20–34.70 | 193.69 |
| Ruby YJIT + Fiber | 30.35 | 32.35 | 29.60–33.65 | 181.20 |

Open-loop load uses absolute scheduled arrivals. Each host has 600 offers at 10 RPS and 2,400 at 40 RPS across three windows. Deadline-bound overload performance is not unconstrained service capacity.

| Host | 10 offered RPS: median completed RPS | 40 offered RPS: median completed RPS | 40 RPS timeouts / 2,400 | 40 RPS successes after window |
| --- | ---: | ---: | ---: | ---: |
| Rust default release | 10.00 | 37.05 | 347 | 60 |
| Ruby plain | 10.00 | 6.45 | 2015 | 0 |
| Ruby YJIT | 10.00 | 33.20 | 204 | 260 |
| Ruby YJIT + Fiber | 10.00 | 24.50 | 839 | 18 |

Across all 51 windows, including the control, **45,771 offers** produced **42,365 verified successes** and **3,405 timeouts**. Successes include **41,919 inside the window** and **446 during drain**. Scheduler/queue drops: 1/0; correctness/HTTP/connection errors: 0/0/0. No failed or drained requests are removed from accounting. Client `sent` counts attempts after connection acquisition, not independent server admission.

## Setup and calibration

One worker uses CPU 2, the common Python asyncio HTTP/1.1 frontend CPU 6, and client/controller CPU 0. Three batches start a fresh pool per host, exclude 50 verified warmups, and run serial seeded-randomized two-second ramps / 20-second windows. Closed concurrency is 1/4. Open load uses 64 connections, at most 256 outstanding operations, a five-second deadline and bounded drain. The frontend has a 512-request queue. AST/source caches persist, request state stays fresh, GC is enabled, and engine responses are never cached. Fiber mode wraps a synchronous render on one Ruby thread and supplies no CPU parallelism.

Every successful response carries the same **714,432-byte** framed HTML/CSS body. The explicitly byte-cached transport control uses the same framing and full hash-verifying client. Its rates were **356.35, 381.65, 296.05 RPS**; median control / fastest engine window was **8.81**, exceeding the required fivefold headroom. Even the minimum control / fastest engine was 7.32. The earlier four-worker pilot failed calibration and supports no scaling claim.

The shared i9-9900K workstation's one-minute load ranged **3.36–12.86**. Affinity does not reserve cores, remove sibling/system load or control frequency. The calibration gate does not establish identical uncontended transport conditions for each serial window. CPU snapshots start after READY; per-window bounds include client setup, ramp, measurement and drain. Pool request counts aggregate all windows. Instantaneous RSS differs from lifetime VmHWM, which includes startup/warmup; neither is per-request retained memory. Three-batch bootstrap intervals are descriptive.

This experiment fetches no gRPC data during measured requests. Native correctness runs warm the service projection cache and use different Ruby/Rust fetch boundaries; see [native data timing boundaries](https://github.com/ebursa91/liquid-rust/blob/codex/horizon-mock-store/compat/horizon/DATA_SERVICE.md). Internal larger-page render-wall, worker CPU and prepared HTTP throughput are separate workloads. Do not invert CPU/render medians into RPS or combine separately executed checkpoints into a precise speedup. Exact fixture/theme/output pins remain unchanged from the historical run below.

Reproduce with [the fork's HTTP harness and method](https://github.com/ebursa91/liquid-rust/blob/codex/horizon-mock-store/compat/horizon/RPS.md), selecting `--worker-counts 1 --batches 3 --duration 20 --ramp 2 --warmup 50 --concurrency-multipliers 1 4 --rates-one 10 40 --headroom 5`. Prepare dependencies/builds outside measurement; preserve all distributions, calibration and error/drain accounting. Live RPC throughput, wider catalogs and calibrated multi-worker scaling remain separate measurements.

---

# Earlier regex-only HTTP checkpoint, 2026-10-01

The release Rust host with compiled regex reuse reached **25.85 verified RPS** at four closed-loop clients. Ruby 4.0.7 YJIT reached **28.90 RPS**; its Fiber wrapper reached **34.05 RPS** in this run. All hosts rendered the same original synthetic homepage into exactly the same bytes. This shared-host experiment compares configured hosts, not languages generally. It does not establish a Fiber speed advantage.

[Complete numeric samples and accounting](results/2026-10-01-rps-after.json) retain every batch and latency sample. Rust implementation: `a468a7ac18155f98fb9fe51881eafac50bae8105`; Ruby: `7db1cc962c1a90a40613da98fdff18f061680bb8`. The data service is excluded: this experiment uses the original prepared offline fixture.

## Closed-loop results

RPS is successful, independently hash-verified HTTP responses completed inside each nominal 20-second window, divided by 20. Tables report the median of three independent window rates. Range retains all three batches. Latency p95 pools completed responses, including responses drained after the window.

| Host | One client RPS | Four clients RPS | Four-client batch range | Four-client p95 ms |
| --- | ---: | ---: | ---: | ---: |
| Rust release | 24.15 | 25.85 | 21.95–27.25 | 257.06 |
| Ruby plain | 13.65 | 14.20 | 13.75–14.75 | 385.03 |
| Ruby YJIT | 26.50 | 28.90 | 23.05–33.00 | 319.63 |
| Ruby YJIT + Fiber | 31.90 | 34.05 | 33.60–36.00 | 176.33 |

## Scheduled offered load

Open-loop requests follow absolute arrival times rather than waiting for the preceding response. Each rate offers 600 requests per host across three windows at 10 RPS, or 2,400 at 40 RPS. The five-second deadline, finite outstanding limit and post-window drain are part of this workload. Timeout-heavy overload rates are not unconstrained service capacity.

| Host | 10 offered RPS: median completed RPS | 40 offered RPS: median completed RPS | 40 RPS timeouts / 2,400 | 40 RPS successes after window |
| --- | ---: | ---: | ---: | ---: |
| Rust release | 10.00 | 18.00 | 1325 | 0 |
| Ruby plain | 9.95 | 6.90 | 2051 | 0 |
| Ruby YJIT | 10.00 | 33.90 | 204 | 163 |
| Ruby YJIT + Fiber | 10.00 | 32.85 | 158 | 266 |

There were no timeouts at 10 offered RPS. Across all 51 windows, including the transport control, 46,728 offers produced 42,990 verified successes and 3,738 timeouts. Of the successes, 42,445 completed inside their windows and 545 during drain. There were no scheduler/queue drops, digest failures, HTTP failures or connection errors. Client `sent` counts send attempts after connection acquisition; it is not an independent server-admission counter.

## Setup and calibration

One engine worker runs on CPU 2, the common Python asyncio HTTP/1.1 frontend on CPU 6, and the client/controller on CPU 0. Each host/batch starts a fresh pool, excludes 50 verified warmup renders, and uses two-second ramps before each of four 20-second windows. Hosts run serially in fixed-seed randomized order. The frontend uses a bounded 512-request queue; open load uses 64 connections and at most 256 outstanding operations. AST/source caches remain prepared; every engine response uses fresh request state. GC stays enabled. The Fiber mode wraps one synchronous Ruby render on one thread and supplies no CPU parallelism.

Every successful response transports the same 714,432-byte length-framed HTML/CSS payload. The explicitly byte-cached transport control uses that payload and the same hash-verifying client. Its three rates were **364.95, 370.65 and 398.85 RPS**. Median control / fastest engine window was **10.02**, above the required fivefold headroom; even the lowest control was 9.86 times that window. A separate four-worker pilot failed the headroom gate and supports no scaling claim.

The shared i9-9900K workstation had one-minute load ranging 3.88–14.68. Affinity does not reserve cores or control CPU frequency. Server request counters aggregate each variant/batch pool. Worker/frontend CPU snapshots start after READY and exclude initialization/warmup; per-window bounds span client setup, ramps, windows and drain. Instantaneous RSS and process-lifetime peak RSS have distinct scopes; peak RSS includes startup/warmup. These metrics do not provide allocation or retained-memory rankings. Three-batch bootstrap intervals in the JSON are descriptive, not controlled-machine confidence bounds.

## Earlier baseline and causal evidence

The [earlier native-context implementation run](results/2026-10-01-rps-before.json), Rust `63df2051d8501a80b764681f7c797ea0907ba718`, measured four-client medians of 11.35 Rust, 13.25 plain Ruby, 29.05 YJIT and 22.40 Fiber RPS. Its control gate passed at 7.14, but individual control batches ranged 113.85–371.65 RPS and host load reached 31.34. These separately executed runs do **not** establish a precise before/after RPS speedup.

The paired [worker CPU investigation](PERFORMANCE_INVESTIGATION.md) provides the causal comparison for regex reuse: 68.5 to 31.5 ms/request, with identical outputs and serial randomized variants in each batch. CPU averages cannot be inverted into HTTP RPS.

The workload uses the original four-product store, pinned unmodified external Horizon homepage, and pinned Ruby Liquid source. Expected HTML is 435,304 bytes with SHA256 `d97c35b3ba08f026536cb4c469623acb9957af59fb9a9171db012958515fe990`; CSS is 279,112 bytes with SHA256 `67a6538e0b763c32ced001728ebf68f375dec497d4fb91c0ee136658ff9f2034`. There is no normalization, engine-output substitution, RPC fetch or real commerce traffic.

Reproduce using [the RPS harness and method](https://github.com/ebursa91/liquid-rust/blob/a468a7ac18155f98fb9fe51881eafac50bae8105/compat/horizon/RPS.md), explicitly choosing `--worker-counts 1 --batches 3 --duration 20 --warmup 50`. Keep transport calibration, all distributions, errors and source/binary fingerprints. Live RPC throughput, larger-page performance and properly calibrated multi-worker scaling remain separate experiments.
