#!/usr/bin/env python3
"""Validate the persisted synthetic store without network access or dependencies."""
import json
import re
import sys
from pathlib import Path
from urllib.parse import urlparse


def require(condition, message):
    if not condition:
        raise ValueError(message)


def validate(path):
    fixture = json.loads(Path(path).read_text(encoding="utf-8"))
    require(fixture["schema_version"] == 1 and fixture["synthetic"] is True, "fixture envelope")
    require(fixture["theme"]["sha"] == "5acd1b6b66c02f61d3216e3adace5dd9e0404fc9", "Horizon pin")
    globals_ = fixture["globals"]
    require(globals_["current_tags"] is None, "homepage has no blog or collection tag context")
    products = globals_["all_products"]
    collections = globals_["collections"]
    require(len(products) == 4, "four product cases")
    require(globals_["shop"]["products_count"] == len(products), "shop product count")
    require(globals_["shop"]["collections_count"] == len(collections), "shop collection count")
    require(globals_["shop"]["currency"] == globals_["cart"]["currency"]["iso_code"] == "USD", "currency consistency")
    variants = {}
    product_ids = {}
    for handle, product in products.items():
        require(product["handle"] == handle and product["url"] == f"/products/{handle}", f"product handle {handle}")
        require(product["id"] not in product_ids, "unique product ids")
        product_ids[product["id"]] = product
        require(product["variants_count"] == len(product["variants"]), f"variant count {handle}")
        prices = [v["price"] for v in product["variants"]]
        require(product["price"] == product["price_min"] == min(prices), f"minimum price {handle}")
        require(product["price_max"] == max(prices), f"maximum price {handle}")
        require(product["price_varies"] == (len(set(prices)) > 1), f"price variance {handle}")
        require(product["available"] == any(v["available"] for v in product["variants"]), f"availability {handle}")
        for variant in product["variants"]:
            require(variant["id"] not in variants, "unique variant ids")
            variants[variant["id"]] = (product, variant)
            require(variant["product"]["id"] == product["id"], "variant product reference")
            require(variant["available"] == (variant["inventory_quantity"] > 0), "inventory availability")
            require(type(variant["price"]) is int and variant["price"] >= 0, "integer subunit prices")
            require(variant["compare_at_price"] is None or type(variant["compare_at_price"]) is int, "compare price type")
            require(len(variant["options"]) == len(product["options"]), "variant options")
            require(variant["options_with_values"] == [{"name": n, "value": v} for n, v in zip(product["options"], variant["options"])], "named variant options")
        image_ids = {image["id"] for image in product["images"]}
        require(product["featured_image"]["id"] in image_ids, "featured image reference")
        for image in product["images"]:
            require(image["product_id"] == product["id"], "image product reference")
            require(image["aspect_ratio"] == image["width"] / image["height"], "image dimensions")
            require(all(v["id"] in variants for v in image["variants"]), "image variant references")
        for collection in product["collections"]:
            require(collection["id"] == collections[collection["handle"]]["id"], "product collection reference")
        first = next((v for v in product["variants"] if v["available"]), None)
        require(product["first_available_variant"] == first, "first available variant")
        require(product["selected_variant"] is None, "no explicit variant selection")
        require(product["selected_or_first_available_variant"] == (first or product["variants"][0]), "variant fallback")
        for option in product["options_with_values"]:
            for value in option["values"]:
                canonical = next(v for v in product["variants"] if v["id"] == value["variant"]["id"])
                require(all(canonical[k] == v for k, v in value["variant"].items()), "option value variant projection")

    for handle, collection in collections.items():
        require(collection["handle"] == handle, "collection handle")
        require(collection["products_count"] == collection["all_products_count"] == len(collection["products"]), "collection counts")
        for product in collection["products"]:
            require(product == products[product["handle"]], "equal canonical product snapshots")
            require(any(c["handle"] == handle for c in product["collections"]), "bidirectional collection membership")

    def validate_links(links):
        handles = set()
        for link in links:
            expected_handle = re.sub(r"[^a-z0-9]+", "-", link["title"].lower()).strip("-")
            require(link["handle"] == expected_handle and link["handle"] not in handles, "synthetic link handle")
            handles.add(link["handle"])
            require(link["data_sharing_opt_out_icon"] is None, "ordinary link has no privacy opt-out icon")
            if link["type"] == "collection_link":
                require(link["object"] == collections[link["object"]["handle"]], "link collection snapshot")
            if link["type"] == "page_link":
                require(link["object"] == globals_["pages"][link["object"]["handle"]], "link page snapshot")
            if link["object"] is not None:
                require(link["url"] == link["object"]["url"], "link object URL")
            validate_links(link["links"])

    for handle, menu in globals_["linklists"].items():
        require(menu["handle"] == handle, "menu handle")
        validate_links(menu["links"])

    cart = globals_["cart"]
    require(cart["item_count"] == sum(item["quantity"] for item in cart["items"]), "cart quantity total")
    for item in cart["items"]:
        product, variant = variants[item["variant_id"]]
        require(item["product_id"] == product["id"] and item["product"] == product, "cart product snapshot")
        require(item["variant"] == variant, "cart variant snapshot")
        require(variant["available"] and item["quantity"] <= variant["inventory_quantity"], "cart stock")
        require(item["final_line_price"] == item["final_price"] * item["quantity"], "cart line total")
        require(item["original_line_price"] == item["original_price"] * item["quantity"], "cart original line total")
    total = sum(item["final_line_price"] for item in cart["items"])
    require(cart["total_price"] == cart["items_subtotal_price"] == cart["checkout_charge_amount"] == total, "cart totals")
    require(cart["original_total_price"] - cart["total_discount"] == total, "cart discounts")
    require(cart["total_weight"] == sum(i["grams"] * i["quantity"] for i in cart["items"]), "cart weight")
    require(products["field-notebook"]["available"] is True, "in-stock case")
    require(products["trail-enamel-mug"]["compare_at_price"] > products["trail-enamel-mug"]["price"], "sale case")
    require(products["wool-camp-blanket"]["available"] is False, "sold-out case")
    require(len(products["canvas-daypack"]["variants"]) == 4, "multivariant case")
    locale = globals_["localization"]
    require(locale["country"] in locale["available_countries"], "current country membership")
    require(locale["language"] in locale["available_languages"], "current language membership")
    require(globals_["request"]["locale"] == locale["language"], "request locale")
    require(globals_["request"]["design_mode"] is False and globals_["request"]["visual_preview_mode"] is False, "live-style request context")

    def validate_fake_hosts(node):
        if isinstance(node, dict):
            for value in node.values():
                validate_fake_hosts(value)
        elif isinstance(node, list):
            for value in node:
                validate_fake_hosts(value)
        elif isinstance(node, str) and node.startswith(("http://", "https://")):
            host = urlparse(node).hostname
            require(host is not None and host.endswith((".example", ".test")), "store URLs use fictitious domains")

    validate_fake_hosts(globals_)
    print(f"fixture valid: {len(products)} products, {len(variants)} variants, {len(collections)} collections, {cart['item_count']} cart units, {total} USD cents")


if __name__ == "__main__":
    try:
        validate(sys.argv[1] if len(sys.argv) > 1 else Path(__file__).with_name("store.json"))
    except (KeyError, TypeError, ValueError) as error:
        sys.exit(f"invalid fixture: {error}")
