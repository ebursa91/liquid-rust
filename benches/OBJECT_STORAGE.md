# Object storage lifecycle controls

`Object::clone` shares an immutable map. Mutable accessors detach shared storage as needed
before returning a mutable reference; nested objects detach when their own accessors
are used. Arrays retain their existing `Vec<Value>` storage. No public iterator,
entry, serialization, equality, `Send` or `Sync` contract changes. Equality still
compares values, including existing non-reflexive NaN behavior.

Empty objects defer map storage allocation. First writes create a fresh map;
read-only empty iterators use an immutable empty map to retain their concrete
HashMap-backed iterator types. Unique owning iteration consumes the map; shared
owning iteration clones its values while preserving other owners.

This favors repeated reads and narrow edits. It adds an Arc allocation to fresh
nonempty objects and reference-count/detachment work to mutation. It is not a
universal speedup: measure construction-heavy and rewrite-heavy applications.

## Reproduction

Build this same benchmark source and manifest registration against both the
baseline (`2e90c658322daf93d8a543b5c1b566ada39a4c19`) and candidate map
implementations, with identical locked dependencies and compiler settings:

```sh
cargo bench --locked --bench object_lifecycle --no-run
```

Run the resulting executables directly, using a different `CRITERION_HOME` for
each window. Run three serialized pairs in shuffled order, with 100 samples,
two seconds of warmup and three seconds of measurement per case:

```sh
CRITERION_HOME=/tmp/liquid-lifecycle-window taskset -c 2 \
  /path/to/object_lifecycle-benchmark --bench --sample-size 100 \
  --warm-up-time 2 --measurement-time 3 --noplot
```

Fresh construction samples include construction, mutation, owning consumption
and drop. Clone-and-mutate samples include cloning, detachment, owning consumption
and drop together. Assigned lookup setup remains outside samples. Shape, values and
retained sources are independently checked outside timing. No rendered-response
cache is involved. Inputs are generic deterministic values, not theme assets.

## Observed results

One Linux x86_64 Intel Core i9-9900K machine, Rust 1.98.1, unchanged Cargo bench
profile: optimization level 3, fat LTO and one codegen unit. Benchmarks ignore
the workspace's release `panic=abort` setting, as described in the
[Cargo profile documentation](https://doc.rust-lang.org/cargo/reference/profiles.html#panic).
The worker ran on physical core 2; the controller ran on separate physical core
0. Affinity does not reserve cores, SMT siblings or CPU frequency.

Values below are medians of three per-window Criterion medians. The baseline and
candidate used the same benchmark source (SHA-256
`75eb9cca39ab215612750bd498495b38425d39a6fc8f219fe1529c059e5ef6ea`).
The candidate map SHA-256 is
`a6d1240dba59090f129182c62f9e2704d0d0bdabf68b859794234ebaf17448cf`.
Executed binaries and all tracked/untracked source files were fingerprinted
before and after the six windows, with no changes. This is a warm iteration
cost comparison on one machine, not HTTP throughput or a portability guarantee.
The [recorded estimates, samples and source fingerprints](object_lifecycle_results.json)
allow the arithmetic and inputs to be checked without publishing machine paths.

| Lifecycle | Baseline | Candidate | Change |
|---|---:|---:|---:|
| Fresh 0-key object, mutation attempt, consume/drop | 12.0 ns | 7.4 ns | -38.2% |
| Fresh 1-key object, mutate, consume/drop | 76.1 ns | 96.7 ns | +27.0% |
| Fresh 2-key object, mutate, consume/drop | 106.5 ns | 131.1 ns | +23.0% |
| Fresh 8-key object, mutate, consume/drop | 534.3 ns | 498.7 ns | -6.7% |
| Assigned full-object lookup, own/drop | 20.069 us | 0.060 us | -99.7% |
| Assigned nested scalar lookup, own/drop | 154.2 ns | 140.8 ns | -8.7% |
| Assigned scalar control, own/drop | 61.1 ns | 57.1 ns | -6.4% |
| Clone, one deep mutation, consume/drop | 20.033 us | 1.050 us | -94.8% |
| Clone, replace most scalar leaves, consume/drop | 25.100 us | 29.686 us | +18.3% |

The full-object and mutation cases use a retained 14-field object with 24 nested
items and independently built expected values. The one-deep-mutation result
illustrates avoiding unrelated copies. Rewriting most fields detaches nearly
the whole graph and costs about 4.6 us more here. Fresh 1/2-key cases cost about
21/25 ns more. These tradeoffs must remain visible when interpreting read-heavy
rendering improvements.

## Validation limits

The candidate adds coverage for every mutable entry point, nested objects and
arrays, owned iteration, retained assigned results, serde, NaN equality, empty
storage and `Send`/`Sync`. Rust 1.83 production library checks cover the changed
crate. The existing locked `snapbox 1.2.2` dev dependency requires edition 2024,
so the Rust 1.83 dev test suite remains a pre-existing toolchain limitation;
the minimum version and dependency lockfile are unchanged.

## Follow-up boundaries

This implementation uses the same detachment rule at every mutable entry point.
It can still copy a shared map before a missing mutable lookup/removal or a
clear operation. Avoiding those copies is a separate optimization requiring
measurements of successful lookups and preservation of unique-map capacity
reuse. It is not included in the results above.

Replacing nearly every field necessarily detaches most shared maps. Improving
empty-map handling does not remove that tradeoff; accepting this change is a
choice to favor repeated reads and narrow mutations.
