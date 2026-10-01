# Shared storefront data over gRPC

The private [horizon-store-service](https://github.com/ebursa91/horizon-store-service/pull/1) owns the complete synthetic store/configuration projection. Native Ruby and Tonic Rust clients consume the same versioned protobuf contract, verify tenant/store/cart/page/locale scope and exact-byte SHA256, and fail without a local fixture fallback. The render hosts apply bounded platform object behavior while preserving the authoritative input bytes.

The service supports two synthetic tenants, 4/100-product stores, en/de/pl locales, published/wide configurations, default/empty carts and genuine pinned index/product/collection templates. Large stores expose page 2 and the final collection page. New contexts contain a complete saved settings profile and `theme.configuration_source=service`; both renderers skip local `config/settings_data.json.current`. Theme schema defaults and template definitions remain pinned implementation code.

Run the service from its private repository according to its README. Then build the independent transport client:

```sh
cargo build --release --locked --manifest-path compat/horizon/grpc-client/Cargo.toml
export HORIZON_STORE_TOKEN=mock-tenant-a-token
compat/horizon/grpc-client/target/release/horizon-grpc-client \
  --endpoint http://127.0.0.1:50051 --tenant-id demo-a --storefront-id large \
  --locale pl --configuration wide --page collection --current-page 2 \
  --cart-id default --request-id request-0001 \
  --renderer "$RUST_ROOT/target/release/examples/horizon" \
  --theme-root "$HORIZON_ROOT" --output-dir /tmp/horizon-rpc-rust
```

The client uses a bounded native RPC call, then passes the verified JSON directly to the Rust renderer through `--fixture-stdin`. It does not create a local store file or patch settings. It records fetch time separately from the renderer child process time. The transport crate requires Rust 1.88+ independently of the Liquid workspace's existing MSRV. Fetch deadlines default to five seconds and are bounded to ten; the separately bounded renderer deadline kills/reaps its owned child. A failed operation removes both success reports.

The Ruby renderer exposes `--grpc-endpoint` directly; see its [client documentation](https://github.com/ebursa91/horizon-ruby-renderer/blob/main/docs/DATA_SERVICE.md). Its SDK and the Rust client have actual loopback gRPC tests. All three repositories mirror the same original `proto/horizon/store/v1/store_context.proto`; the service repository is the contract authority.

This is a mock read service on loopback with explicit fake credentials. The authenticator binds a principal before tenant lookup, and projections/cache keys include tenant/store/cart scope. The private ADR describes provider, identity and command-API boundaries for future multi-tenant ecommerce. Real customer sessions, remote transport security, payments, inventory reservations and mutations remain future work.

## Reproduce native parity

`compare_rpc.py` uses the private service's generated Python gRPC stub as a context oracle, then invokes each independent native renderer client. It checks unchanged input/source revisions, the expected ready-line snapshot revision, context stability across correlation IDs, tenant catalog identities, cart totals, genuine executed sections, observable locale/canonical/variant/pagination facts and exact HTML/CSS bytes. It repeats representative requests in fresh processes. No normalization or rendered-response caching is allowed.

```sh
PYTHONPATH="$SERVICE_ROOT/src" "$SERVICE_ROOT/.venv/bin/python" compat/horizon/compare_rpc.py \
  --ruby-root "$RUBY_ROOT" --rust-root "$RUST_ROOT" --service-root "$SERVICE_ROOT" \
  --liquid-root "$LIQUID_ROOT" --theme-root "$HORIZON_ROOT" \
  --rust-binary "$RUST_ROOT/target/release/examples/horizon" \
  --rust-client "$RUST_ROOT/compat/horizon/grpc-client/target/release/horizon-grpc-client" \
  --ruby "$RUBY" --ruby-gem-home "$RUBY_GEMS" --ruby-bundle-path "$RUBY_BUNDLE" \
  --endpoint 127.0.0.1:50051 --expected-revision "$READY_SNAPSHOT_REVISION" \
  --output-dir /tmp/horizon-rpc-comparison
```

All source repositories must be clean. Output stays outside them; rendered theme source/output remains local. Publish only the numeric/hash comparison report. `diagnostic_timing` includes CLI startup/dependency activation, RPC fetch, validation and rendering; it is not an RPS benchmark or an equal-work startup comparison. The historical `baseline` query preserves the original fixture bytes and legacy settings behavior only for explicit oracle reproduction. Prepared renderer HTTP benchmarks use that offline baseline and exclude data-service work; they are documented separately in `RPS.md`.
