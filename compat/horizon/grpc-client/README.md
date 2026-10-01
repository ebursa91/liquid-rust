This standalone client fetches the authoritative mock store/configuration snapshot with native Tonic gRPC and gives its exact bytes to the Rust Horizon renderer over stdin. It has its own Cargo workspace and lockfile; its modern transport dependencies do not change liquid-rust's dependencies or MSRV. Build with a current Rust toolchain and `protoc` on PATH:

```sh
cargo build --locked --manifest-path compat/horizon/grpc-client/Cargo.toml
```

Run the private mock store service first, then provide the token through an environment variable and an already-built Horizon renderer:

```sh
HORIZON_STORE_TOKEN=mock-tenant-a-token \
  compat/horizon/grpc-client/target/debug/horizon-grpc-client \
  --endpoint http://127.0.0.1:50051 \
  --tenant-id demo-a --storefront-id small --locale en \
  --configuration published --page index --current-page 1 \
  --cart-id default --request-id original-demo-request \
  --renderer target/release/examples/horizon \
  --theme-root /absolute/path/to/pinned/horizon \
  --output-dir /tmp/horizon-native-rpc
```

Only explicit loopback HTTP endpoints are accepted. This is an insecure local mock transport, not production tenant authentication. The client verifies the echoed scope, selected page and template, pinned theme SHA, deterministic locale/current page, snapshot revision, and SHA256 of the raw JSON. Nonbaseline snapshots must declare service-owned settings and matching JSON service scope. The special original baseline is allowed only for its known exact fixture hash and scope. The SHA detects inconsistent bytes; it is not a cryptographic signature.

There is no local snapshot fallback or response cache. The renderer retains its existing mock platform contracts and reads implementation schemas/defaults from the pinned theme. Service settings bypass the theme's persisted `settings_data.json.current` completely.

`--timeout-ms` defaults to 5000 and is bounded to 1–10000 ms; this includes connecting and fetching, with a transmitted gRPC deadline. `--render-timeout-ms` defaults to 60000 and is bounded to 1–300000 ms; timed-out renderer children are killed and reaped. `fetch_ms` reports connect/fetch time before hash/schema verification. `renderer_process_ms` includes process startup and artifact writes, so neither is an engine-only benchmark.

Success produces local `index.html`, `styles.css`, `report.json`, and `rpc-report.json`. Both success markers are removed before a new fetch, so a failed request cannot be mistaken for a previous successful render. Output paths with symlinks or collisions with inputs are rejected before removal or writes. Reports contain scoped metadata and digests, not tokens or store snapshots. Theme source and generated HTML/CSS remain local and are not vendored here.

```sh
cargo test --locked --manifest-path compat/horizon/grpc-client/Cargo.toml
cargo clippy --locked --all-targets --manifest-path compat/horizon/grpc-client/Cargo.toml -- -D warnings
```

The standalone transport client requires Rust 1.88 or newer, matching its locked Tonic dependencies. The Liquid workspace keeps its existing MSRV.
