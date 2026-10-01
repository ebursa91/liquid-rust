# Profiling Liquid rendering

Enable the optional `profiling` feature to emit standard `tracing` spans. The
library installs no subscriber and writes no diagnostics itself. The application
chooses its subscriber, filtering, aggregation and output destination.

```toml
[dependencies]
liquid = { path = "../liquid-rust", features = ["profiling"] }
tracing = { version = "0.1", default-features = false, features = ["std"] }
tracing-subscriber = { version = "0.3", default-features = false, features = ["fmt", "std"] }
```

Until this feature is published, point the path dependency at this checkout. The
example above assumes the application and checkout are adjacent directories.

The [small example](../examples/profiling.rs) uses a scoped formatting subscriber
and ordinary synthetic Liquid. It prints the rendered text to stdout and tracing
diagnostics to stderr:

```bash
cargo run --locked --example profiling --features profiling
```

`tracing::subscriber::with_default(subscriber, || template.render(&globals))`
applies the subscriber only to that synchronous operation and restores the
caller's subscriber afterward. With `liquid-core` directly, enable its `profiling`
feature and install your subscriber around runtime rendering in the same way.

## Spans and fields

Targets begin with `liquid::profile`. DEBUG spans cover template bodies; TRACE
spans cover parsed nodes, output/filter chains, filters, selectors, runtime
lookups, value projections and frames. Selecting DEBUG omits the more frequent
TRACE operations. Applications can filter these targets independently of their
other telemetry.

Fields contain structural metadata: registered tag/filter names, node kind,
parser-buffer ID and coordinates, node/filter/selector counts, lookup mode and
depth, frame kind, and success/found/owned/delegated outcomes. Liquid values,
evaluated variable paths, argument literals, runtime names and error messages are
not fields. Custom application spans and subscribers must apply their own data
policy.

`buffer_id`, `line`, `column` and `byte` identify a parser input buffer. Plugins and
the `liquid` tag may create secondary buffers; those coordinates are not implicitly
external-file lines. IDs are opaque and need not match across parses or runs.
Programmatically constructed renderables and anonymous filters need application
spans for more specific attribution. This API does not implement Ruby Liquid's
profiler interface.

## Interpretation and lifecycle

The example requests entry and close events. A formatting subscriber's busy/idle
span durations measure entered wall time and span lifetime, including diagnostic
overhead and scheduling. They are not CPU samples, allocations, HTTP throughput
or a performance comparison. Inclusive nested durations overlap. Measure
instrumented and uninstrumented builds separately before making speed claims.

Default builds omit the optional tracing dependency, parser wrappers and runtime
hooks. Compiling the feature still adds wrappers when no subscriber is active and
may change code generation. Disabling collection is different from compiling
without the feature.

The scoped dispatcher is local to the current thread. Applications starting new
threads must propagate their dispatcher. Async applications should instrument
futures and propagate their subscriber rather than hold an entered guard across
`await`. Ordinary errors and unwinding close guards; panic abort or forced process
termination cannot guarantee exported diagnostics. Export buffering and resource
limits belong to the application's subscriber.

For independent CPU attribution, use an optimized build with debug information
and a sampling profiler. The optional `profiling` Cargo build profile inherits
release settings and adds line tables; it is separate from the instrumentation
feature. See the [Rust Performance Book](https://nnethercote.github.io/perf-book/profiling.html)
and [tracing documentation](https://docs.rs/tracing/0.1.44/tracing/index.html).
