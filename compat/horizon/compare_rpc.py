#!/usr/bin/env python3
"""Verify native gRPC consumers against one tenant-scoped Horizon data authority."""
import argparse
import hashlib
import json
import math
import os
from pathlib import Path
import subprocess
import sys
import time

from compare_scenarios import (BASE_SHA, LIQUID_SHA, THEME_SHA, check_page,
                               checkout, execute, fingerprint, require, source_names)


def run(args):
    require(math.isfinite(args.timeout) and args.timeout > 0, 'finite positive timeout required')
    require(args.expected_revision.startswith('mock-v1-'), 'expected ready-line service revision required')
    import grpc
    from horizon.store.v1 import store_context_pb2 as messages
    from horizon.store.v1 import store_context_pb2_grpc as rpc

    roots = {name: path.resolve() for name, path in {
        'ruby': args.ruby_root, 'rust': args.rust_root, 'service': args.service_root,
        'liquid': args.liquid_root, 'theme': args.theme_root}.items()}
    pins = {'liquid': LIQUID_SHA, 'theme': THEME_SHA}
    sources = {name: checkout(path, pins.get(name)) for name, path in roots.items()}
    binaries = {name: fingerprint(path) for name, path in {
        'rust_renderer': args.rust_binary, 'rust_grpc_client': args.rust_client}.items()}
    require(fingerprint(roots['ruby'] / 'fixtures/store.json')['sha256'] == BASE_SHA, 'original fixture changed')
    destination = args.output_dir.absolute()
    for parent in (destination, *destination.parents):
        require(not parent.is_symlink(), 'output path contains a symlink')
    require(not destination.exists(), 'fresh output required')
    for root in roots.values():
        require(not destination.resolve().is_relative_to(root), 'output overlaps a source checkout')
    destination.mkdir(parents=True)
    env = os.environ.copy()
    for key in ('RUBYLIB', 'RUBYOPT', 'RUBY_YJIT_ENABLE', 'GEM_HOME', 'GEM_PATH', 'BUNDLE_PATH'):
        env.pop(key, None)
    env.update(BUNDLE_GEMFILE=str(roots['ruby'] / 'Gemfile'), BUNDLE_PATH=str(args.ruby_bundle_path),
               GEM_HOME=str(args.ruby_gem_home), GEM_PATH=str(args.ruby_gem_home))
    ruby = [str(args.ruby), '--disable-yjit', '-rbundler/setup', str(roots['ruby'] / 'bin/horizon-render'),
            '--liquid-root', str(roots['liquid']), '--theme-root', str(roots['theme']), '--scope', 'page',
            '--grpc-endpoint', args.endpoint]
    rust = [str(args.rust_client), '--endpoint', 'http://' + args.endpoint, '--renderer', str(args.rust_binary),
            '--theme-root', str(roots['theme']), '--token-env', 'HORIZON_DATA_TOKEN']
    channel = grpc.insecure_channel(args.endpoint, options=(('grpc.max_receive_message_length', 64*1024*1024+8192),))
    stub = rpc.StoreContextServiceStub(channel)
    records, identities, revisions, html_hashes = [], {}, set(), {}
    try:
        for tenant in ('demo-a', 'demo-b'):
            token = 'mock-tenant-a-token' if tenant == 'demo-a' else 'mock-tenant-b-token'
            env['HORIZON_DATA_TOKEN'] = token
            for storefront, size in (('small', 4), ('large', 100)):
                for locale in ('en', 'de', 'pl'):
                    for configuration, per_page in (('published', 24), ('wide', 12)):
                        last = (size + per_page - 1) // per_page
                        pages = [('index', 1), ('product', 1), ('collection', 1)]
                        if size == 100:
                            pages += [('collection_page_2', 2), ('collection_last', last)]
                        carts = ['default']
                        # Empty carts exercise tenant ownership and zero-value projections across both sizes/settings.
                        if locale == 'en':
                            carts += ['empty']
                        for cart in carts:
                            for page, number in pages:
                                if cart == 'empty' and page not in ('index', 'product'):
                                    continue
                                key = '-'.join(map(str, (tenant, storefront, locale, configuration, page, number, cart)))
                                fields = dict(tenant_id=tenant, storefront_id=storefront, locale=locale,
                                              configuration=configuration, page=page, current_page=number,
                                              cart_id=cart, request_id='matrix-' + key)
                                def fetch(correlation):
                                    request = messages.GetRenderContextRequest(**dict(fields, request_id=correlation))
                                    response = stub.GetRenderContext(request, timeout=5, metadata=(('authorization', 'Bearer ' + token),))
                                    for name, value in dict(fields, request_id=correlation).items():
                                        require(getattr(response, name) == value, 'reference response scope differs')
                                    require(hashlib.sha256(response.context_json).hexdigest() == response.context_sha256, 'reference context digest differs')
                                    return response
                                first = fetch(fields['request_id'])
                                second = fetch('repeat-' + key)
                                require(first.context_json == second.context_json and first.snapshot_revision == second.snapshot_revision,
                                        'correlation ID changes authoritative deterministic data')
                                fixture = json.loads(first.context_json)
                                scope = fixture['manifest']['service_scope']
                                require(scope == {name: value for name, value in fields.items() if name != 'request_id'}, 'body service scope differs')
                                require(fixture['theme']['configuration_source'] == 'service', 'saved configuration authority differs')
                                require(len(fixture['globals']['all_products']) == size and fixture['globals']['shop']['products_count'] == size, 'catalog count differs')
                                expected_domain = 'harbor-store.example' if tenant == 'demo-a' else 'cedar-store.example'
                                require(fixture['globals']['shop']['domain'] == expected_domain, 'tenant shop identity differs')
                                actual_cart = fixture['globals']['cart']
                                require(actual_cart['item_count'] == sum(line['quantity'] for line in actual_cart['items']), 'cart quantities differ')
                                require(actual_cart['total_price'] == sum(line['final_line_price'] for line in actual_cart['items']), 'cart values differ')
                                require(actual_cart['checkout_charge_amount'] == actual_cart['total_price'], 'mock checkout amount differs')
                                require(cart != 'empty' or actual_cart['item_count'] == actual_cart['total_price'] == actual_cart['total_weight'] == 0, 'empty cart totals differ')
                                require(str(fixture['pages'][page]['request_id']) == fixture['globals']['request']['id'], 'deterministic rendering request ID differs')
                                require(first.snapshot_revision == args.expected_revision, 'running service differs from expected ready-line revision')
                                revisions.add(first.snapshot_revision)
                                product_ids = tuple(sorted(product['id'] for product in fixture['globals']['all_products'].values()))
                                identities[tenant, storefront] = set(product_ids)
                                expected, provenance, timing = None, {}, {}
                                # All axes render independently. Repeat representative requests in fresh processes.
                                repeats = 2 if storefront == 'small' and locale == 'en' and configuration == 'published' and cart == 'default' else 1
                                for engine, command in (('ruby', ruby), ('rust', rust)):
                                    times = []
                                    for repeat in range(repeats):
                                        output = destination / key / f'{engine}-{repeat}'
                                        options = []
                                        for name, value in fields.items():
                                            options.extend(['--' + name.replace('_', '-'), str(value)])
                                        options.extend(['--output-dir', str(output)])
                                        started = time.monotonic()
                                        artifacts, report = execute(command + options, output, env, args.timeout)
                                        wall_ms = (time.monotonic() - started) * 1000
                                        metadata = report['data_service'] if engine == 'ruby' else json.loads((output / 'rpc-report.json').read_text())
                                        require(metadata['context_sha256'] == first.context_sha256 and metadata['snapshot_revision'] == first.snapshot_revision, 'client snapshot differs from service reference')
                                        require(metadata.get('local_fixture_fallback') is False, 'client fallback contract differs')
                                        require(report['theme_sha'] == THEME_SHA and report['fixture_sha256'] == first.context_sha256, 'renderer input pin differs')
                                        require(engine != 'ruby' or report['liquid_sha'] == LIQUID_SHA, 'Ruby Liquid pin differs')
                                        require(expected is None or artifacts == expected, f'exact output differs: {key}/{engine}-{repeat}')
                                        expected = artifacts
                                        facts = check_page((output / 'index.html').read_text(), fixture, page)
                                        names = source_names(report)
                                        if fixture['pages'][page]['type'] in ('product', 'collection'):
                                            section = 'sections/product-information.liquid' if fixture['pages'][page]['type'] == 'product' else 'sections/main-collection.liquid'
                                            require(section in names, 'genuine pinned page section was not executed')
                                        times.append({'cli_wall_ms': wall_ms, 'fetch_ms': metadata['fetch_ms']})
                                        provenance[engine] = {'executed_sources': len(names), 'transport': metadata.get('transport', 'native grpc'),
                                                              'runtime': {name: report[name] for name in ('ruby_version', 'yjit_enabled', 'build_profile') if name in report}}
                                    timing[engine] = times
                                html_hashes[tenant, storefront, locale, configuration, page, cart] = expected['html']['sha256']
                                records.append({'tenant_id': tenant, 'storefront_id': storefront, 'locale': locale, 'configuration': configuration,
                                                'page': page, 'current_page': number, 'cart_id': cart,
                                                'context': {'bytes': len(first.context_json), 'sha256': first.context_sha256},
                                                'request_id': fixture['globals']['request']['id'], 'product_count': size,
                                                'cart_units': actual_cart['item_count'], 'cart_total_cents': actual_cart['total_price'],
                                                'artifacts': expected, 'observable_page': facts, 'provenance': provenance,
                                                'repeated_renders_per_engine': repeats, 'diagnostic_timing': timing})
                                print('verified ' + key, flush=True)
        for storefront in ('small', 'large'):
            require(identities['demo-a', storefront].isdisjoint(identities['demo-b', storefront]), 'tenant product identities overlap')
            for page in ('index', 'product', 'collection'):
                for locale in ('en', 'de', 'pl'):
                    for configuration in ('published', 'wide'):
                        a = html_hashes['demo-a', storefront, locale, configuration, page, 'default']
                        b = html_hashes['demo-b', storefront, locale, configuration, page, 'default']
                        require(a != b, 'tenant page output does not differ')
        for tenant in ('demo-a', 'demo-b'):
            for storefront in ('small', 'large'):
                for page in ('index', 'product', 'collection'):
                    for configuration in ('published', 'wide'):
                        require(len({html_hashes[tenant, storefront, locale, configuration, page, 'default'] for locale in ('en', 'de', 'pl')}) == 3,
                                'locale outputs do not differ')
                    for locale in ('en', 'de', 'pl'):
                        require(html_hashes[tenant, storefront, locale, 'published', page, 'default'] != html_hashes[tenant, storefront, locale, 'wide', page, 'default'],
                                'configuration outputs do not differ')
                    if page in ('index', 'product'):
                        for configuration in ('published', 'wide'):
                            require(html_hashes[tenant, storefront, 'en', configuration, page, 'default'] != html_hashes[tenant, storefront, 'en', configuration, page, 'empty'],
                                    'cart outputs do not differ')
        current = {name: checkout(path, pins.get(name)) for name, path in roots.items()}
        require(current == sources, 'source drift during comparison')
        require(len(revisions) == 1, 'service projection revision changed during comparison')
        require(binaries == {name: fingerprint(path) for name, path in {'rust_renderer': args.rust_binary, 'rust_grpc_client': args.rust_client}.items()}, 'binary drift')
        summary = {'schema_version': 1, 'correctness_verified': True, 'transport': 'native grpc',
                   'sources': sources, 'binaries': binaries, 'theme_sha': THEME_SHA, 'liquid_sha': LIQUID_SHA,
                   'original_fixture_sha256': BASE_SHA, 'snapshot_revision': next(iter(revisions)),
                   'tenants': 2, 'locales': ['en', 'de', 'pl'], 'product_counts': [4, 100],
                   'configurations': ['published', 'wide'], 'page_cases': len(records),
                   'verified_renders': sum(record['repeated_renders_per_engine']*2 for record in records),
                   'context_determinism_verified': True, 'exact_html_css_parity': True, 'cases': records,
                   'limits': ['CLI timing includes process/dependency startup, context validation and rendering; diagnostic only, not RPS',
                              'native gRPC fetch timing is reported separately from CLI wall time',
                              'loopback synthetic authentication and immutable carts; production commerce/authentication requires further work',
                              'external theme and generated HTML/CSS remain local']}
        (destination / 'comparison.json').write_text(json.dumps(summary, indent=2, allow_nan=False) + '\n')
    finally:
        channel.close()


def arguments():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ('ruby-root', 'rust-root', 'service-root', 'liquid-root', 'theme-root', 'rust-binary', 'rust-client', 'ruby', 'ruby-gem-home', 'ruby-bundle-path', 'output-dir'):
        parser.add_argument('--' + name, type=Path, required=True)
    parser.add_argument('--endpoint', required=True)
    parser.add_argument('--expected-revision', required=True)
    parser.add_argument('--timeout', type=float, default=120)
    return parser.parse_args()


if __name__ == '__main__':
    try:
        run(arguments())
    except (ValueError, OSError, subprocess.SubprocessError, KeyError) as error:
        print(str(error), file=sys.stderr)
        sys.exit(1)
