# Horizon synthetic storefront

Render the same entirely invented store JSON with Ruby Liquid and liquid-rust,
using unmodified external Shopify Horizon sources. This is a local
interoperability fixture, with bounded mock Shopify platform adapters.

`store.json` contains four products (regular, sale, sold out, multiple variants),
seven variants, three collections, navigation, request/locale and a nonempty
cart. All store data and the product SVG assets are synthetic. See
[FIXTURE.md](FIXTURE.md) for types, consistency rules and the distinction between
our envelope and Shopify's Liquid object conventions.

## References and running

| Reference | Required revision |
| --- | --- |
| Shopify Liquid 5.14.0 | `4e39ae4cc3da73921923c0669e0fc84a66b2f696` |
| Shopify Horizon 4.2.0 | `5acd1b6b66c02f61d3216e3adace5dd9e0404fc9` |
| Rust baseline | `4d57f1941b6b93c951b5e7dfc6caf2407dea32b2` |

Provide clean external checkouts at those revisions, install the Ruby reference's
Bundler dependencies, and use the workspace Rust toolchain. No Shopify account,
API access, merchant data or credentials are needed. The comparer makes no
network requests; Cargo/Bundler may need cached dependencies.

```bash
export LIQUID_RUBY_ROOT=/absolute/path/to/pinned-liquid
export HORIZON_THEME_ROOT=/absolute/path/to/pinned-horizon
export BUNDLE_GEMFILE="$LIQUID_RUBY_ROOT/Gemfile"
python3 compat/horizon/validate_fixture.py
bundle exec ruby compat/horizon/ruby/test_renderer.rb
cargo test --locked --example horizon
python3 compat/horizon/compare.py \
  --liquid-root "$LIQUID_RUBY_ROOT" \
  --theme-root "$HORIZON_THEME_ROOT" \
  --scope page --output-dir /tmp/horizon-comparison
```

`hero` renders the real hero/text/button blocks; `template` renders the ordered
real index sections and all four product cards; `page` adds the real layout,
header/announcement/footer groups, cart and search markup. The engines render
independently, with the same JSON and theme sources. Ruby uses strict parsing,
`render!`, strict filters and optional variables. Rust uses explicit mock host
adapters around the public parser/runtime extension interfaces.

Each engine runs twice. The comparer checks HTML and collected CSS byte for byte,
without trimming, rewriting theme sources or substituting Ruby output into Rust.
A failure exits nonzero and removes any previous success manifest. Success writes
`comparison.json` with input pins, fixture digest, output sizes/digests and engine
Git state. Engine reports inventory executed sources/platform filters. Generated
HTML/CSS stay outside source checkouts; the repository contains original fixture
and host code only.

For local previews, serve each successful engine directory separately so absolute
asset URLs resolve correctly:

```bash
python3 -m http.server 8011 --bind 127.0.0.1 --directory /tmp/horizon-comparison/ruby
python3 -m http.server 8012 --bind 127.0.0.1 --directory /tmp/horizon-comparison/rust
```

Theme assets link to the external pinned checkout, and the product SVGs are copied
from our mock assets. Keep Horizon sources and generated theme outputs local;
review the external theme's LICENSE.md before any distribution or deployment.

## Scope and known limits

Mock hosts provide bounded section/block ordering, schema defaults, dynamic
settings, snippet scopes, stylesheet collection, colors/fonts, image/assets,
translations, money, forms, pagination and analytics payloads needed by this
fixture. Unsupported executed operations fail. The fixture explicitly disables
hosted payment terms; real checkout, Shopify services and browser commerce
interactions are outside this local rendering MVP.

Core fixes are separate from these adapters: `liquid`, `echo`, inline comments,
ASCII tag whitespace, question-mark properties, literal-prefix variable names,
`default: ..., allow_false: ...`,
right-associated mixed logical conditions, nil multiplication coercion, selected
clamp numeric types, decimal fractional multiplication, `find_index` and Ruby
decimal/scientific float formatting.
Optional expression lookup is opt-in through `Runtime::strict_variables` and
`RuntimeBuilder::set_strict_variables(false)`; ordinary library rendering stays
strict, and direct `Runtime::get` keeps its error behavior. The host's optional
lookup and render-global contracts do not establish full engine parity. Core error diagnostics currently include generated statement
locations for `liquid`; exact Ruby error wording/line formatting, comment-block
argument syntax, ordinary quoted `%}` tokenization and broader numeric coercion
remain separate compatibility work. Decimal multiplication uses the stdlib-only
`bigdecimal` dependency and the already locked `ryu` formatter to match Ruby's
canonical decimal operands before returning a float;
other arithmetic filters retain their existing coercion limitations. Rare shortest-decimal ties can still differ
between Rust and Ruby; this fixture does not exercise those values.

This fixture exercises one homepage configuration and locale. Passing it does
not establish universal Liquid compatibility, complete Shopify Drop semantics,
other Horizon templates, responsive interactions or hosted Shopify behavior.

## Prepared rendering and performance

The original Ruby fixture host above remains a correctness baseline. Its
standalone modern Ruby implementation is maintained in
[ebursa91/horizon-ruby-renderer](https://github.com/ebursa91/horizon-ruby-renderer),
with the same synthetic fixture bytes and external reference pins.

The Rust example prepares immutable source, schema, asset and parsed partial
caches once, then creates fresh Liquid runtimes and clears stylesheet, platform
call and executed-source state for every request. It uses `LazyCompiler` to
retain parsed ASTs. Rendered responses are never cached. Immutable store values are borrowed through ObjectView maps; small Arc snapshots retain only request/platform overrides, preserving caller-assignment isolation without copying the complete store into every snippet. The prepared renderer
accepts sequential requests; it does not establish concurrent shared-host safety.

Build outside timing and run the release worker directly:

```sh
cargo build --release --locked --example horizon
./target/release/examples/horizon \
  --theme-root "$HORIZON_THEME_ROOT" --fixture compat/horizon/store.json \
  --scope page --output-dir /tmp/horizon-rust-worker \
  --benchmark-json /tmp/horizon-rust-worker/benchmark.json \
  --iterations 30 --warmup 50 --benchmark-mode direct
```

Each warmup and measured request renders independently. The worker checks HTML
and collected CSS SHA-256 digests outside the render timer, fails on changing
outputs and records each measured request. Initialization excludes lazy AST
compilation, which is exercised by warmup. Warm times include fresh request
runtimes, diagnostics and both output buffers; hashing and artifact/report writes
are excluded. Process startup and cold initialization require separate external
measurements. Debug workers identify themselves and must not be used for release
performance claims. SHA-256 uses the already locked, dev-only `sha2` dependency.

The four-mode comparison tool and its timing boundaries are documented in
[BENCHMARKING.md](BENCHMARKING.md). It activates the standalone Ruby lockfile
inside each worker, verifies every warm result against independent output bytes,
and records cold CLI timings separately from warm prepared rendering.

The [seven-batch performance comparison](RESULTS.md) covers release Rust and Ruby 4.0.7 in plain, YJIT and Fiber/YJIT modes, with every output checked. Warm medians in that shared-host run were 65.66, 55.42, 23.75 and 24.37 ms respectively; see the complete distributions and limitations before interpreting them. The standalone original Ruby renderer is public at [ebursa91/horizon-ruby-renderer](https://github.com/ebursa91/horizon-ruby-renderer). The [rendering and performance goal](ROADMAP.md) records the next coverage and measurement steps.
