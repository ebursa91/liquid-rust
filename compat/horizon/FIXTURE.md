# Synthetic Horizon store fixture

`store.json` is our persisted fake store for both renderers. Its envelope is our
fixture convention, not an official Shopify whole-store JSON export. No data came
from a merchant, customer, Shopify API, or live service. The store and product copy
are invented. IDs, timestamps, ordering, prices, inventory and request context are
fixed. Both engines must parse the same file and keep its JSON types.

The reference theme is external Shopify Horizon at
`5acd1b6b66c02f61d3216e3adace5dd9e0404fc9`. No theme source is included here.
The theme repository/SHA in `theme` are provenance metadata. Use a verified local
checkout at that SHA when rendering; the fixture does not fetch anything.

## Envelope and rendering contract

- `schema_version: 1` and `synthetic: true` identify this fixture format.
- `manifest` records our description, fixed date and product cases; do not expose
  it as a Liquid object.
- `globals` supplies Liquid values. `all_products` and `collections` are keyed by
  handle, `linklists` by menu handle, and `pages` by page handle.
- `theme.settings` supplies saved global settings. The original offline baseline
  merges schema defaults, pinned `config/settings_data.json.current`, then these
  overrides. New gRPC contexts declare `theme.configuration_source=service` and
  provide the complete saved profile; the host skips local saved configuration.
- `theme.section_overrides[id].settings` and
  `theme.block_overrides[id].settings` override the real index template's settings
  after their relevant schema defaults and template settings. Retain the real
  ordered section/block definitions; these maps do not replace theme source.
- `pages.index` describes the render target, not Shopify's `page` object. Render
  the real `templates/index.json`; an index request has no current `page` object.
- Resolve a collection-picker handle such as `all` to
  `globals.collections.all` when building section settings, and a menu-picker
  handle to `globals.linklists[handle]`. Resolve the theme's explicit dynamic
  setting bindings before rendering. These are platform adapter responsibilities.

The pilot overrides the hero overlay off, uses an ordinary button, and sets our
own heading/button text and colors. They avoid the default custom-button hover
branch while preserving actual section/block/snippet evaluation. The product list
uses the four products in collection `all`. Assets referenced by synthetic data
use local `/cdn/shop/...` paths. Absolute store URLs and email use `.example`.
The adapters must not contact these addresses or invent successful responses for
unsupported executed platform operations.

## Product and cart cases

| Handle | Case | Variants | Price in USD cents |
|---|---|---:|---:|
| `field-notebook` | In stock | 1 | 1800 |
| `trail-enamel-mug` | Sale, compare at 3200 | 1 | 2400 |
| `wool-camp-blanket` | Sold out, inventory denied | 1 | 6800 |
| `canvas-daypack` | Color/Size options, one unavailable combination | 4 | 8500–9500 |

The cart contains two notebooks and one large sand daypack: three units, two
lines, 13100 cents, 800 grams, no cart discount. Compare-at pricing is not a cart
discount. Product, variant and image IDs agree across collection views, cart
lines and navigation. `price`/`price_min`/`price_max`, availability, selected-or-first
variant, named options and collection counts are materialized consistently.

This is a finite JSON value snapshot of documented fields, not a complete Shopify
Drop implementation. Cyclic relationships use finite identity projections:
`variant.product` carries product identity; `product.collections` carries collection
identity; image-associated variants and product-option-value variants carry
documented variant fields. Canonical products remain in `globals.all_products`;
collection product arrays and cart product/variant snapshots equal those values.
Navigation links include deterministic title-slug `handle` strings at every
level, so the original theme can build stable drawer IDs. Ordinary links carry
`data_sharing_opt_out_icon: null`; this store has no data-sale opt-out link.
The homepage has `current_tags: null`: Shopify exposes tag context on blog and
collection pages, and an empty array would incorrectly trigger the theme title
branch because Liquid arrays are truthy. No `$ref` or engine-specific mutation is
required to consume the file. Fields
outside those finite projections remain unsupported until deliberately modeled.

JSON objects do not reproduce Shopify Drop stringification or method lookup.
For example, image-to-string, case-insensitive product-option lookup, font/color
objects and dynamic platform services need explicit shared adapters when executed.
Do not treat nil fields or omitted unsupported fields as proof that those services
are implemented. Customer, policies, selling plans, discounts, store pickup,
metafields and metaobjects are deliberately absent/empty in this store.

`manifest.platform_capabilities.payment_terms=false` explicitly disables the
installment-banner service for this fixture. A bounded adapter can return empty
markup only for that declared disabled capability; it must reject an enabled
capability it cannot emulate. `globals.powered_by_link` is synthetic static HTML
with a `.example` destination. Shopify's normal field links to Shopify; our
fixture intentionally uses a fake provider and does not call or endorse a service.

## Validation

Run from the repository root:

```sh
python3 compat/horizon/validate_fixture.py
```

The dependency-free validator checks counts, ID references, full snapshot equality,
collection membership in both directions, price ranges, stock, option references,
cart arithmetic/weight, navigation handles/privacy-icon nulls, locale consistency,
fictitious URLs and the theme pin.
It uses no network. It validates our fixture integrity; it does not validate the
entire Shopify object model or establish engine compatibility.

## Field provenance

Property naming and JSON value types were checked against Shopify's official
Liquid documentation on 2026-09-30 and 2026-10-01. All sample values and prose are our own.
The money values are integers in currency subunits; `shop.currency` is a string,
while `cart.currency` and `country.currency` are currency objects. Timestamps are
fixed strings with an explicit UTC offset; no clock is read during rendering.

- [Product](https://shopify.dev/docs/api/liquid/objects/product),
  [variant](https://shopify.dev/docs/api/liquid/objects/variant),
  [product option](https://shopify.dev/docs/api/liquid/objects/product_option),
  [product option value](https://shopify.dev/docs/api/liquid/objects/product_option_value).
- [Image](https://shopify.dev/docs/api/liquid/objects/image),
  [media](https://shopify.dev/docs/api/liquid/objects/media),
  [image presentation](https://shopify.dev/docs/api/liquid/objects/image_presentation),
  [focal point](https://shopify.dev/docs/api/liquid/objects/focal_point),
  [collection](https://shopify.dev/docs/api/liquid/objects/collection).
- [Shop](https://shopify.dev/docs/api/liquid/objects/shop),
  [cart](https://shopify.dev/docs/api/liquid/objects/cart),
  [line item](https://shopify.dev/docs/api/liquid/objects/line_item).
- [Link](https://shopify.dev/docs/api/liquid/objects/link),
  [linklist](https://shopify.dev/docs/api/liquid/objects/linklist),
  [page](https://shopify.dev/docs/api/liquid/objects/page),
  [template](https://shopify.dev/docs/api/liquid/objects/template),
  [current tags](https://shopify.dev/docs/api/liquid/objects/current_tags).
- [Localization](https://shopify.dev/docs/api/liquid/objects/localization),
  [country](https://shopify.dev/docs/api/liquid/objects/country),
  [currency](https://shopify.dev/docs/api/liquid/objects/currency),
  [shop locale](https://shopify.dev/docs/api/liquid/objects/shop_locale),
  [market](https://shopify.dev/docs/api/liquid/objects/market).
- [Routes](https://shopify.dev/docs/api/liquid/objects/routes),
  [request](https://shopify.dev/docs/api/liquid/objects/request),
  [powered-by link](https://shopify.dev/docs/api/liquid/objects/powered_by_link),
  [payment terms](https://shopify.dev/docs/api/liquid/filters/payment_terms),
  [JSON templates](https://shopify.dev/docs/storefronts/themes/architecture/templates/json-templates),
  [settings](https://shopify.dev/docs/api/liquid/objects/settings).
