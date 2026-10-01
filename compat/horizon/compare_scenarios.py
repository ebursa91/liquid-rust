#!/usr/bin/env python3
"""Compare genuine pinned Horizon pages across deterministic synthetic scenarios."""
import argparse
import hashlib
from html.parser import HTMLParser
import json
import math
import os
from pathlib import Path
import shutil
import signal
import subprocess
import sys

LIQUID_SHA = "4e39ae4cc3da73921923c0669e0fc84a66b2f696"
THEME_SHA = "5acd1b6b66c02f61d3216e3adace5dd9e0404fc9"
BASE_SHA = "867c41e0929881290af2b261af64146632bf0287524f5a6c55714af32e819f98"
ARTIFACTS = {"html": "index.html", "css": "styles.css"}


def require(condition, message):
    if not condition:
        raise ValueError(message)


def fingerprint(path):
    path = Path(path)
    require(path.is_file() and not path.is_symlink(), f"regular input required: {path}")
    content = path.read_bytes()
    return {"bytes": len(content), "sha256": hashlib.sha256(content).hexdigest()}


def checkout(path, pin=None):
    path = Path(path).resolve()
    def git(*args):
        return subprocess.check_output(["git", "-C", str(path), *args], text=True).strip()
    require(Path(git("rev-parse", "--show-toplevel")) == path, "repository root required")
    head, status = git("rev-parse", "HEAD"), git("status", "--porcelain", "--untracked-files=all")
    require(not status, f"clean source checkout required: {path}")
    require(pin is None or head == pin, f"reference revision differs: {path}")
    return {"head": head, "clean": True}


class PageFacts(HTMLParser):
    def __init__(self, html):
        super().__init__(convert_charrefs=True)
        self.lang, self.canonicals, self.cards, self.variants, self.schemas = None, [], [], [], []
        self.schema = None
        self.feed(html)
        self.close()

    def handle_starttag(self, tag, attributes):
        attrs = dict(attributes)
        if tag == "html":
            self.lang = attrs.get("lang")
        elif tag == "link" and attrs.get("rel") == "canonical":
            self.canonicals.append(attrs.get("href"))
        elif tag == "li" and "product-grid__item" in (attrs.get("class") or "").split():
            self.cards.append((int(attrs["data-product-id"]), int(attrs["data-page"])))
        if "data-current-variant-id" in attrs:
            self.variants.append(int(attrs["data-current-variant-id"]))
        if tag == "script" and attrs.get("type") == "application/ld+json":
            self.schema = []

    def handle_data(self, data):
        if self.schema is not None:
            self.schema.append(data)

    def handle_endtag(self, tag):
        if tag == "script" and self.schema is not None:
            self.schemas.append(json.loads("".join(self.schema)))
            self.schema = None


def check_page(html, fixture, page_name):
    """Verify observable locale, route, selected variant and collection window."""
    page = fixture["pages"][page_name]
    facts = PageFacts(html)
    require(facts.lang == fixture["manifest"]["locale"], "rendered locale differs")
    require(facts.canonicals == [page["canonical_url"]], "rendered canonical route differs")
    resource = page.get("resource", {})
    if resource.get("type") == "collection":
        products = fixture["globals"]["collections"][resource["handle"]]["products"]
        per_page = 12 if fixture["manifest"]["configuration"] == "wide" else 24
        number = page.get("current_page", 1)
        expected = [(product["id"], number) for product in products[(number - 1) * per_page:number * per_page]]
        require(facts.cards == expected, f"collection page {number} cards differ")
    elif resource.get("type") == "product":
        product = fixture["globals"]["all_products"][resource["handle"]]
        require(facts.variants and set(facts.variants) == {resource["variant_id"]}, "selected variant differs")
        groups = [entry for entry in facts.schemas if isinstance(entry, dict) and entry.get("@type") == "ProductGroup"]
        require(len(groups) == 1 and groups[0].get("name") == product["title"], "product structured data differs")
    return {"locale": facts.lang, "canonical_url": facts.canonicals[0], "collection_cards": len(facts.cards),
            "current_page": page.get("current_page", 1), "selected_variant_ids": sorted(set(facts.variants))}


def source_names(report):
    sources = report.get("sources", [])
    return [source if isinstance(source, str) else source.get("path") for source in sources]


def execute(command, output, env, timeout):
    output.mkdir(parents=True, exist_ok=False)
    with (output / "process.log").open("wb") as log:
        process = subprocess.Popen(command, stdout=log, stderr=subprocess.STDOUT, env=env, start_new_session=True)
        try:
            status = process.wait(timeout=timeout)
            if status:
                raise subprocess.CalledProcessError(status, command)
        finally:
            # The native Rust client owns a renderer child; stop the entire owned group.
            try:
                os.killpg(process.pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
            process.wait()
    artifacts = {key: fingerprint(output / name) for key, name in ARTIFACTS.items()}
    report = json.loads((output / "report.json").read_text())
    return artifacts, report


def run(args):
    require(math.isfinite(args.timeout) and args.timeout > 0, "finite positive timeout required")
    sources = {"ruby": checkout(args.ruby_root), "rust": checkout(args.rust_root),
               "liquid": checkout(args.liquid_root, LIQUID_SHA), "theme": checkout(args.theme_root, THEME_SHA)}
    require(fingerprint(args.base)["sha256"] == BASE_SHA, "original fixture changed")
    require(fingerprint(args.ruby_root / "fixtures/store.json") == fingerprint(args.base), "base fixtures differ")
    binary = fingerprint(args.rust_binary)
    destination = args.output_dir.absolute()
    for root in (args.ruby_root, args.rust_root, args.liquid_root, args.theme_root):
        require(not destination.resolve().is_relative_to(root.resolve()), "output must stay outside all source checkouts")
    for parent in (destination, *destination.parents):
        require(not parent.is_symlink(), "output path contains a symlink")
    require(not destination.exists(), "fresh output directory required")
    destination.mkdir(parents=True)
    env = os.environ.copy()
    for key in ("RUBYLIB", "RUBYOPT", "RUBY_YJIT_ENABLE", "GEM_HOME", "GEM_PATH", "BUNDLE_PATH"):
        env.pop(key, None)
    env["BUNDLE_GEMFILE"] = str(args.ruby_root / "Gemfile")
    if args.ruby_gem_home:
        env["GEM_HOME"] = str(args.ruby_gem_home)
    if args.ruby_bundle_path:
        env["BUNDLE_PATH"] = str(args.ruby_bundle_path)
    if args.ruby_gem_path:
        env["GEM_PATH"] = str(args.ruby_gem_path)
    ruby = [str(args.ruby), "--disable-yjit", "-rbundler/setup"]
    generated = destination / "fixtures"
    subprocess.run([*ruby, str(args.ruby_root / "script/generate_scenarios.rb"), "--base", str(args.base), "--output-dir", str(generated)], check=True, env=env)
    second = destination / "fixture-repeat"
    subprocess.run([*ruby, str(args.ruby_root / "script/generate_scenarios.rb"), "--base", str(args.base), "--output-dir", str(second)], check=True, env=env)
    fixtures = sorted(generated.glob("*.json"))
    require(len(fixtures) == 12, "expected all twelve locale/configuration/size fixtures")
    subprocess.run([*ruby, str(args.ruby_root / "script/validate_scenario.rb"), *map(str, fixtures)], check=True, env=env)
    records, request_ids, hashes = [], set(), {}
    for path in fixtures:
        fixture_hash = fingerprint(path)
        require(fixture_hash == fingerprint(second / path.name), "fixture regeneration differs")
        fixture = json.loads(path.read_text())
        scenario = fixture["manifest"]["scenario_id"]
        for page_name, page in fixture["pages"].items():
            require(page["request_id"] not in request_ids, "duplicate deterministic page request ID")
            request_ids.add(page["request_id"])
            expected = None
            provenance = {}
            facts = None
            for engine in ("ruby", "rust"):
                for repeat in range(2):
                    output = destination / scenario / page_name / f"{engine}-{repeat}"
                    options = ["--theme-root", str(args.theme_root), "--fixture", str(path), "--page", page_name,
                               "--scope", "page", "--output-dir", str(output)]
                    command = [*ruby, str(args.ruby_root / "bin/horizon-render"), "--liquid-root", str(args.liquid_root), *options] if engine == "ruby" else [str(args.rust_binary), *options]
                    artifacts, report = execute(command, output, env, args.timeout)
                    require(report.get("theme_sha") == THEME_SHA and report.get("fixture_sha256") == fixture_hash["sha256"], "renderer input fingerprints differ")
                    require(engine != "ruby" or report.get("liquid_sha") == LIQUID_SHA, "Ruby Liquid pin differs")
                    require(expected is None or artifacts == expected, f"independent/repeated output differs: {scenario}/{page_name}/{engine}-{repeat}")
                    expected = artifacts
                    facts = check_page((output / "index.html").read_text(), fixture, page_name)
                    names = source_names(report)
                    if page.get("type") in ("product", "collection"):
                        section = "sections/product-information.liquid" if page["type"] == "product" else "sections/main-collection.liquid"
                        require(section in names, "genuine pinned page section was not executed")
                    provenance[engine] = {"executed_sources": len(names), "platform_calls": report.get("platform_calls"),
                                          "runtime": {key: report[key] for key in ("ruby_version", "yjit_enabled", "build_profile") if key in report}}
            hashes[(fixture["manifest"]["product_count"], fixture["manifest"]["locale"], fixture["manifest"]["configuration"], page_name)] = expected["html"]["sha256"]
            records.append({"scenario_id": scenario, "fixture": fixture_hash, "page": page_name, "template": page["template"],
                            "request_id": page["request_id"], "product_count": fixture["manifest"]["product_count"],
                            "configuration": fixture["manifest"]["configuration"], "cart_units": fixture["globals"]["cart"]["item_count"],
                            "cart_total_cents": fixture["globals"]["cart"]["total_price"], "artifacts": expected,
                            "observable_page": facts, "provenance": provenance, "repeated_renders_per_engine": 2})
            print(f"verified {scenario}/{page_name}", flush=True)
    for size in (4, 100):
        for page in ("index", "product", "collection"):
            for configuration in ("default", "wide"):
                require(len({hashes[(size, locale, configuration, page)] for locale in ("en", "de", "pl")}) == 3, "locale outputs did not differ")
            for locale in ("en", "de", "pl"):
                require(hashes[(size, locale, "default", page)] != hashes[(size, locale, "wide", page)], "settings outputs did not differ")
    current = {"ruby": checkout(args.ruby_root), "rust": checkout(args.rust_root),
               "liquid": checkout(args.liquid_root, LIQUID_SHA), "theme": checkout(args.theme_root, THEME_SHA)}
    require(current == sources and fingerprint(args.rust_binary) == binary and fingerprint(args.base)["sha256"] == BASE_SHA, "source/input drift during comparison")
    summary = {"schema_version": 1, "correctness_verified": True, "theme_sha": THEME_SHA, "liquid_sha": LIQUID_SHA,
               "base_fixture_sha256": BASE_SHA, "sources": sources, "rust_binary": binary,
               "scenario_fixtures": len(fixtures), "page_cases": len(records), "verified_renders": len(records) * 4,
               "fixture_regeneration_verified": True, "cases": records,
               "limits": ["offline synthetic Shopify host adapters, no live platform services", "original generated mock JSON; external pinned theme and generated HTML/CSS stay local", "correctness matrix is separate from the original homepage RPS workload"]}
    (destination / "comparison.json").write_text(json.dumps(summary, indent=2, allow_nan=False) + "\n")


def arguments():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ("ruby-root", "rust-root", "liquid-root", "theme-root", "rust-binary", "base", "output-dir"):
        parser.add_argument("--" + name, type=Path, required=True)
    parser.add_argument("--ruby", type=Path, required=True)
    parser.add_argument("--ruby-gem-home", type=Path)
    parser.add_argument("--ruby-gem-path", type=Path)
    parser.add_argument("--ruby-bundle-path", type=Path)
    parser.add_argument("--timeout", type=float, default=120)
    return parser.parse_args()


if __name__ == "__main__":
    try:
        run(arguments())
    except (ValueError, OSError, subprocess.SubprocessError, KeyError) as error:
        print(str(error), file=sys.stderr)
        sys.exit(1)
