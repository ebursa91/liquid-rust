# Rendering and performance goal

Maintain one persisted, entirely synthetic Shopify-shaped store that Ruby and Rust render into identical bytes using a pinned, unmodified external Horizon theme. Expand supported pages and configurations only with independently checked output contracts. Keep Rust implementation and PRs in the user fork; external theme sources and generated theme output stay local.

1. Repeat the homepage benchmark on a quiet machine, assess longer YJIT warmup, and profile remaining Rust render costs. Retain cold/warm boundaries, all process windows, actual versions and every-output checks; require measured benefit before adopting an optimization.
2. Add genuine pinned product and collection templates using original mock JSON. Cover additional locales, settings/configurations and larger stores with deterministic request identifiers and cart values.
3. Measure realistic concurrent requests separately from this serial benchmark. Preserve fresh request state and cooperative Fiber isolation; choose schedulers/workers according to actual I/O/CPU work and test errors/cancellation. No concurrency claim follows from wrapping one synchronous render in a Fiber.
4. Broaden executed Shopify platform contracts and core Liquid regressions without treating unavailable services or fixture stubs as universal compatibility. Keep correctness, compiler/feature checks, independent review and versioned benchmark evidence in each delivery.

The completed homepage benchmark is the first performance checkpoint. It does not complete broad Shopify rendering or establish a universal engine ranking.
