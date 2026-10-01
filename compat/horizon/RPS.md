# Verified HTTP requests per second

`rps.py` measures complete local HTTP requests against prepared Ruby and release
Rust renderer processes. Both engines use the same Python asyncio HTTP/1.1 front
end, worker-pool admission, framed pipe protocol, keep-alive client and response
verification. Each successful response contains the original synthetic homepage
HTML and collected CSS. This measures rendering, IPC, HTTP transfer and client
verification together. It is a bounded local host comparison, not a production
Rails/Rack/Rust web-framework benchmark.

## Setup and run

Build the release example and install the selected Ruby's locked dependencies
before measurement. Both independent engines must support the serving worker
protocol. No build, dependency installation, network fetch or global system
tuning occurs inside the harness.

```bash
python3 compat/horizon/rps.py \
  --ruby /absolute/path/to/ruby-4.0.7/bin/ruby \
  --ruby-root /absolute/path/to/horizon-ruby-renderer \
  --ruby-gem-home /absolute/path/to/ruby-4.0-gems \
  --ruby-gem-path /absolute/path/to/ruby-4.0-gems \
  --liquid-root /absolute/path/to/pinned-liquid \
  --theme-root /absolute/path/to/pinned-horizon \
  --rust-root /absolute/path/to/liquid-rust \
  --rust-binary /absolute/path/to/liquid-rust/target/release/examples/horizon \
  --fixture /absolute/path/to/liquid-rust/compat/horizon/store.json \
  --expected-comparison /tmp/horizon-comparison/comparison.json \
  --worker-counts 1 4 --worker-cpus 2 3 4 5 \
  --frontend-cpu 6 --client-cpu 0 \
  --duration 20 --ramp 2 --batches 3 --warmup 50 \
  --concurrency-multipliers 1 4 \
  --rates-one 10 40 --rates-four 40 160 \
  --output-dir /tmp/horizon-rps
```

Choose rates transparently after an untimed pilot, then keep the same absolute
rates for all four variants within a topology. Record the chosen configuration;
do not tune each engine's offered load separately. The defaults give two
closed-loop connection levels (`workers` and `4 * workers`) and two open-loop
rates per topology. `--concurrency` can explicitly replace those levels. A
command smoke check can use `--duration 1 --ramp .1 --batches 1 --warmup 1
--worker-counts 1 --concurrency 1 --rates-one 10`; it supports no performance
conclusion.

The four actual variants are Rust release, plain Ruby, Ruby YJIT, and Ruby YJIT
inside a Fiber. Ruby loads `bundler/setup` in-process; the serving worker reports
the actual version, dependencies and YJIT state. Each renderer performs the
configured startup warmup with fresh requests and checks the independent
homepage oracle before announcing readiness. After startup it retains parsed
AST/source/schema caches, creates fresh request state, renders HTML and CSS, and
checks output sizes. The common HTTP client hashes both bodies on every response.
Rendered responses are never cached by the actual engines. A Fiber remains one
serial request on one worker thread.

On the recorded i9-9900K, logical CPUs `0/8` through `7/15` are sibling pairs.
Workers use CPU 2 for one process or CPUs 2, 3, 4, 5 for four. The common front end
uses CPU 6 and the independent client CPU 0. The harness verifies physical package
and core IDs through Linux sysfs and rejects overlap between assigned roles.
Change these arguments to fit another machine. Affinity does not reserve CPUs,
control frequency scaling, quiet sibling applications or remove host load.

## Measurements and accounting

Each batch starts a new prepared server/worker pool for every variant and
topology. All startup warmups are excluded. A short verified HTTP ramp precedes
each measured window, reusing those keep-alive connections during measurement.
Cases and windows are shuffled with the recorded seed, then run serially.
All TCP connections are established before ramp. An open-loop ramp uses only
one concurrent request per renderer process, so a large connection pool does not
inject an excluded overload burst immediately before the measured offered rate.

The throughput numerator is only verified HTTP 200 bodies completed within the
declared measurement window. Divide that count by the declared window seconds.
Requests completing afterward remain in the drain count and latency samples;
they never inflate RPS. Throughput is measured directly, not derived from the
reciprocal of a render-time median.

Closed-loop clients keep a fixed number of requests in flight and start the next
request only after the previous response. This shows achieved capacity for that
concurrency. It has coordinated omission: arrivals slow down when the server is
slow, so its latency distribution does not represent a fixed external arrival
rate.

Open-loop offers have fixed intended arrival timestamps independent of response
completion. All planned offers count, including drops. An offer delayed by more
than one arrival interval is recorded as a scheduler drop instead of replaying an
unbounded catch-up burst. Outstanding tasks are capped; excess offers become
queue drops. Connection-pool waiting is included in intended-arrival-to-body
latency. Report scheduler lag and these drops alongside server results; client
overload does not demonstrate renderer capacity.

The client records offered, started, sent, successful, completed-in-window,
post-window successful, HTTP failures, connection errors, timeouts and drops.
The front end separately records admitted requests, queue timeouts, completed
renders, sent bodies and client disconnects. Latency p50/p95/p99 retains requests
that finish during bounded drain, including completed error responses. Successful
latencies are also reported separately so quick rejections do not hide slow
successful responses. Timeouts are censored failures with counts and observed
timeout durations; dropped offers have no response latency. Do not remove those
failures or drained requests when presenting results.

The render-worker pool is common, with identical configured queue and timeout
bounds. Idle keep-alive timeout is separate from request timeout. Disconnects
after a client deadline are accounted without treating ordinary overload as a
renderer correctness failure. Worker protocol/render failures, wrong output
lengths, body hash differences and source/input drift invalidate the experiment.

## Validate transport headroom

An explicit transport-only worker caches the already verified local HTML/CSS
bytes and uses the same framed IPC, front end, HTTP body and hashing client.
It is labeled `transport-control` and `response_cache: true`; it is not an engine
result. The control has no renderer warmup and receives the same HTTP ramp.

At each topology, the median measured control RPS must exceed the strongest
observed engine window by at least five times. `--headroom` can require a larger
factor. A lower ratio sets `publication_valid: false`: the common plumbing may
limit capacity, so improve the setup or report a harness bottleneck instead of
an engine ranking. The same client must validate the same 714,432-byte framed
body (16-byte length header plus 435,304 HTML and 279,112 CSS bytes). Any lower
payload or skipped digest check would make this control misleading.

## Artifacts and limits

`rps.json` contains all original numeric request counts, per-window RPS, latency
samples and percentiles, errors/drop counts, timing/drain boundaries, worker
readiness, source/binary fingerprints, topology, load and headroom. Summary
throughput uses independent batch windows; bootstrap intervals are descriptive
and do not remove shared-host uncertainty. Pool latency percentiles from original
samples rather than averaging percentiles across windows. p99 with a small
sample count has substantial uncertainty.

Linux `/proc` snapshots report cumulative process CPU seconds and current/peak
RSS for client, front end and workers at window boundaries. They do not measure
allocations or supply a cross-engine allocation claim. Inherited environment
variables are passed to child processes and never serialized; only explicit
nonsecret Ruby runtime configuration is recorded. Raw output remains outside
checkouts. Generated Horizon HTML/CSS and private raw configuration/log files
must not be committed or uploaded as public metrics.

Every worker and client belongs to an owned process group. Timeout, failed
startup and early front-end exit stop descendants. Normal shutdown drains
admitted requests before closing workers. A stale `rps.json` marker is removed
before measurement, and a new report records headroom validity explicitly.

The measured workload is one configured synthetic Shopify homepage and bounded
platform adapters. The shared host's CPU load and scheduling remain uncontrolled.
Results support only observed throughput/latency for the recorded configuration,
not universal Liquid, Shopify or web-framework performance.

Run the lightweight framing, HTTP, accounting and descendant-cleanup tests with:

```bash
python3 -m unittest discover -s compat/horizon -p test_rps.py -v
```

These use tiny synthetic bodies and loopback only. An environment that blocks
local socket binding needs permission for loopback access before they can run.
