# HTTP throughput checkpoint, 2026-10-01

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
