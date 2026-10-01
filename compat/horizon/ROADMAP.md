# Rendering and performance goal

Build a tenant-scoped storefront data authority that supplies both Ruby and Rust with the same entirely synthetic store, cart and saved configuration. Both hosts must render identical bytes from genuine pinned, unmodified external Horizon templates. Keep Rust changes and PRs in the user fork. External theme sources and generated HTML/CSS remain local.

## Delivered checkpoint

Native Ruby and Tonic Rust clients consume one private mock gRPC service. The authority binds a principal before tenant lookup, owns every saved configuration value, and returns bounded, revisioned contexts with exact-byte hashes and explicit request/cart/page scope. The clients validate the contract and fail without a local file fallback.

The final correctness matrix verifies 112 scopes and 236 fresh-process renders across two tenants, 4/100 products, en/de/pl, published/wide settings, index/product/collection, pagination and representative empty carts. A separate deterministic offline matrix verifies 48 page cases and 192 renders. Both produce exact HTML/CSS parity, preserving all earlier context/cart/page goldens.

The performance work removed repeated regex compilation, then shared immutable section/closest snapshots instead of repeatedly copying their store trees. The 100-product collection's render-wall batch medians improved by 25.8–28.5×; requested new-allocation bytes fell 94.5%. The original homepage shows a smaller CPU gain. Proper one-worker HTTP experiments include transport calibration, closed concurrency, scheduled offered load, deadlines, errors and drain accounting. All failed candidates, distributions, source/binary fingerprints and independent reviews are retained. These measurements rank the configured synthetic hosts only.

## Next measurements and implementation

1. Profile the optimized host before choosing another change. Investigate mutable-frame lookup ownership and sorted JSON materialization only when measured costs justify a safe design. Keep typed properties, explicit nil, assignment isolation and exact-output gates. Isolated Path/loop/keyword experiments and ThinLTO did not establish a consistent benefit and remain deferred.
2. Measure live RPC fetch/validation separately from prepared rendering and full HTTP request throughput. The native matrix warms the immutable projection cache and has different Ruby/Rust fetch boundaries; it supports correctness, not SDK rankings or service capacity. Develop bounded page projections before much wider catalogs.
3. Calibrate multi-worker HTTP scaling with enough transport headroom; repeat on a quieter machine and assess YJIT warmup sensitivity. Preserve fresh request state, cancellation and bounded queues. Fibers provide cooperative request isolation on one thread.
4. Evolve the service through typed catalog/configuration/cart providers, consistent revisions, tenant membership and customer cart ownership. Add remote transport security and authorized idempotent command APIs before real commerce use. Keep this read-only mock contract explicit.

Every delivery requires relevant semantic and feature/compiler checks, genuine pinned-theme parity, independent review, committed/pushed increments and versioned evidence. The mock service is an extensible starting point for multi-tenant ecommerce. Current evidence and methods are linked from the public renderer's [results](https://github.com/ebursa91/horizon-ruby-renderer/blob/main/benchmark/RESULTS.md).
