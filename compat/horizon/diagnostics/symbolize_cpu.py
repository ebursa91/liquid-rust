#!/usr/bin/env python3
"""Resolve owned Horizon CPU callchains; keep maps, addresses and source lines local."""
import argparse
import collections
import hashlib
import json
from pathlib import Path
import re
import subprocess


def main(args):
    metadata = json.loads((args.capture / 'metadata.json').read_text())
    if not metadata.get('complete') or not metadata.get('correctness_verified') or metadata['sample_summary']['lost'] != 0:
        raise RuntimeError('capture must be completed, verified and loss-free')
    def capture_files():
        return {name: {'bytes': (args.capture / name).stat().st_size,
                       'sha256': hashlib.sha256((args.capture / name).read_bytes()).hexdigest()}
                for name in ('cpu.samples', 'maps.txt')}
    raw_before = capture_files()
    if raw_before != metadata['capture_files']:
        raise RuntimeError('raw capture files differ from verified metadata')
    binary = Path(metadata['manifest']['binary']).resolve()
    before = hashlib.sha256(binary.read_bytes()).hexdigest()
    if before != metadata['manifest']['binary_fingerprint']['sha256']:
        raise RuntimeError('renderer executable changed after capture')
    header = subprocess.check_output(['readelf', '-h', str(binary)], text=True)
    if not re.search(r'Type:\s+DYN', header):
        raise RuntimeError('this map-offset resolver supports Linux ELF PIE executables only')
    mappings, bases = [], {}
    for line in (args.capture / 'maps.txt').read_text().splitlines():
        fields = line.split(maxsplit=5)
        if len(fields) != 6 or not fields[5].startswith('/'):
            continue
        start, end = (int(value, 16) for value in fields[0].split('-'))
        offset, name = int(fields[2], 16), fields[5]
        bases[name] = min(bases.get(name, start - offset), start - offset)
        mappings.append((start, end, name))
    def resolve(address):
        for start, end, name in mappings:
            if start <= address < end:
                return name, address - bases[name]
        return None
    samples, ips = [], []
    addresses = collections.defaultdict(set)
    for line in (args.capture / 'cpu.samples').read_text().splitlines():
        fields = line.split()
        ip = int(fields[1], 16)
        chain = [int(value, 16) for value in fields[2:] if int(value, 16) < 2**63]
        if not chain or chain[0] != ip:
            chain.insert(0, ip)
        ips.append(resolve(ip))
        frames = []
        for index, address in enumerate(chain):
            point = resolve(address if index == 0 else address - 1)
            if point:
                addresses[point[0]].add(point[1])
                frames.append(point)
        samples.append(frames)
    if not samples or len(samples) != metadata['sample_summary']['samples']:
        raise RuntimeError('sample file count differs from completed capture')
    symbols = {}
    for name, offsets in addresses.items():
        if Path(name).resolve() != binary:
            for offset in offsets:
                symbols[(name, offset)] = [{'function': '<unresolved system code>', 'source': '??:0'}]
            continue
        ordered = sorted(offsets)
        output = subprocess.check_output(['addr2line', '-a', '-f', '-C', '-i', '-e', name],
                    input=''.join(hex(value) + '\n' for value in ordered), text=True).splitlines()
        groups, current, lines = {}, None, []
        for line in output:
            if re.fullmatch(r'0x[0-9a-fA-F]+', line):
                if current is not None:
                    groups[current] = lines
                current, lines = int(line, 16), []
            else:
                lines.append(line)
        if current is not None:
            groups[current] = lines
        if set(groups) != set(ordered):
            raise RuntimeError('symbolizer dropped addresses')
        for offset, lines in groups.items():
            if len(lines) % 2:
                raise RuntimeError('invalid inline frame output')
            symbols[(name, offset)] = [{'function': lines[index], 'source': lines[index + 1]}
                                      for index in range(0, len(lines), 2)]
    self_counts, inclusive = collections.Counter(), collections.Counter()
    unknown = 0
    for frames, point in zip(samples, ips):
        if point is None:
            unknown += 1
        else:
            self_counts[Path(point[0]).name + '::' + symbols[point][0]['function']] += 1
        inclusive.update(set(Path(frame[0]).name + '::' + info['function']
                             for frame in frames for info in symbols[frame]))
    count = len(samples)
    result = {'schema_version': 1, 'total_samples': count, 'samples_without_mapped_ip': unknown,
              'binary_sha256': before,
              'self': [{'function': name, 'samples': value, 'percent': value * 100 / count}
                       for name, value in self_counts.most_common()],
              'inclusive': [{'function': name, 'samples': value, 'percent': value * 100 / count}
                            for name, value in inclusive.most_common()],
              'limits': ['Self counts use sampled user-mode IPs; unmapped IPs are counted separately.',
                         'Inclusive functions are deduplicated per chain; rows overlap and must not be summed.',
                         'Inline renderer frames resolved using addr2line -i; caller return addresses adjusted by one byte.',
                         'System code remains unresolved without debug information; no nearby-export attribution.',
                         'Raw mappings, addresses and source locations remain local.']}
    if capture_files() != raw_before:
        raise RuntimeError('raw capture files changed during symbolization')
    if hashlib.sha256(binary.read_bytes()).hexdigest() != before:
        raise RuntimeError('binary changed during symbolization')
    (args.capture / 'symbolized-local.json').write_text(json.dumps({str(key): value for key, value in symbols.items()}, indent=2) + '\n')
    (args.capture / 'summary.json').write_text(json.dumps(result, indent=2) + '\n')
    print(json.dumps({'samples': count, 'unmapped': unknown, 'top_self': result['self'][:15],
                      'top_inclusive': result['inclusive'][:15]}, indent=2))


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--capture', type=Path, required=True)
    main(parser.parse_args())
