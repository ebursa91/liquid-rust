# Rendering and performance goal

Build a tenant-scoped storefront data authority that supplies both Ruby and Rust with the same entirely synthetic store, cart and saved configuration. Both hosts must render identical bytes from genuine pinned, unmodified external Horizon templates. Keep Rust changes and PRs in the user fork. External theme sources and generated HTML/CSS remain local.

## Delivered checkpoint

Native Ruby and Tonic Rust clients consume one private mock gRPC service. The authority binds a principal before tenant lookup, owns every saved configuration value, and returns bounded, revisioned contexts with exact-byte hashes and explicit request/cart/page scope. The clients validate the contract and fail without a local file fallback.

The correctness matrix verifies 112 scopes and 236 fresh-process renders across two tenants, 4/100 products, en/de/pl, published/wide settings, index/product/collection, pagination and representative empty carts. A separate deterministic offline matrix verifies 48 page cases and 192 renders. Both produce exact HTML/CSS parity.

The performance investigation removed repeated regex compilation and reduced observed Rust worker CPU from 68.5 to 31.5 ms/request. Proper one-worker HTTP experiments include transport calibration, closed concurrency, scheduled offered load, deadlines, errors and drain accounting. Ruby YJIT remains competitive; the measurements rank these configured synthetic hosts only.

## Active work

1. Profile remaining Rust allocations and CPU costs across lookup paths, loop scopes and inherited Shopify context. Compare isolated candidates with unprofiled paired CPU measurements and exact-output checks, then retain only useful, semantics-preserving changes. Publish source/binary fingerprints, distributions and independent review.
2. Extend measured performance to genuine product/collection and larger stores. Measure live RPC fetch/validation separately from prepared rendering and full request throughput. The existing native matrix's mixed fresh-process diagnostics are correctness evidence, not SDK rankings or RPS.
3. Calibrate multi-worker HTTP scaling with enough transport headroom; repeat on a quieter machine and assess YJIT warmup sensitivity. Preserve fresh request state, cancellation and bounded queues. Fibers provide cooperative request isolation, not parallel CPU execution.
4. Evolve the service through typed catalog/configuration/cart providers, consistent revisions, tenant membership and customer cart ownership. Add remote transport security and authorized idempotent command APIs before real commerce use. Keep this read-only mock contract explicit.

Every delivery requires relevant semantic and feature/compiler checks, genuine pinned-theme parity, independent review, committed/pushed increments and versioned evidence. The mock service is an extensible starting point for multi-tenant ecommerce, not a production commerce platform.
