#!/usr/bin/env python3
"""Release resource regression checks on macOS/Linux, against Node v22 yaml@2.9.0.

Run from repo root:
  CARGO_HOME="$PWD/_build/cargo-home" python3 src/pilotdeck-yaml/tests/run_resource_scaling.py \
    --baseline /path/to/baseline --output _tmp/review-yaml/resources-round6.json
Or use the ignored test with PILOTDECK_YAML_BASELINE and cargo test --release
-p pilotdeck-yaml --test resource_scaling resource_scaling -- --ignored --exact --nocapture.

N is calibrated using ONLY Node to 0.2-1s; 4N scales breadth/length/repetitions,
never nesting depth. Each sample gets a fresh process. Median of three timings,
maximum of three process-lifetime peak RSS measurements. No startup/input IO/drop
in timings; input IO, runtime, parser, retained documents/values and teardown in RSS.
Pass: Rust/Node time and absolute RSS <=3 at BOTH sizes; time growth / Node
time growth <=1.5; Rust RSS(4N)-RSS(N) <=1.5 * (Node RSS(4N)-RSS(N)).
Nonzero exits/timeouts are failures, not omitted rows.
"""
import argparse
import json
import hashlib
import platform
import os
from pathlib import Path
import statistics
import sys
import subprocess
import tempfile

HERE = Path(__file__).resolve().parent
REPO = HERE.parents[2]
FAMILIES = ['documents', 'comments', 'plain', 'quoted', 'flow_seq', 'block_map',
            'mixed_500', 'anchors_aliases', 'block_scalar', 'complex_keys', 'tags_directives']
SEEDS = dict(zip(FAMILIES, [50000, 100000, 2000000, 200000, 100000, 3000,
                           100, 10000, 100000, 1, 40000]))


def source(family, n):
    if family == 'documents':
        return '---\n' * n
    if family == 'comments':
        return '# c\n' * n
    if family == 'plain':
        return 'word ' * n + '\n'
    if family == 'quoted':
        return '"' + 'a\\n\\t\\u0041\\U0001F600' * n + '"\n'
    if family == 'flow_seq':
        return '[' + ', '.join(['x'] * n) + ']\n'
    if family == 'block_map':
        return ''.join(f'k{i}: x\n' for i in range(n))
    if family == 'mixed_500':
        # A fixed block seq/map prefix, then 498 alternating flow seq/map levels.
        prefix = ''.join('[' if i % 2 == 0 else '{k: ' for i in range(498))
        suffix = ''.join(']' if i % 2 == 0 else '}' for i in reversed(range(498)))
        return ('---\n- ? ' + prefix + '0' + suffix + '\n') * n
    if family == 'anchors_aliases':
        return '[' + ', '.join(f'&a{i} x, *a{i}' for i in range(n)) + ']\n'
    if family == 'block_scalar':
        return '|\n' + '  content\n' * n
    if family == 'complex_keys':
        # Fixed-depth complex block keys; scale repetitions, not extreme depth.
        return ('---\n' + '? ' * 200 + '0\n') * n
    if family == 'tags_directives':
        return (''.join(f'%TAG !t{i}! tag:x{i}:\n' for i in range(n)) + '---\n'
                + ''.join(f'- !t{i}!value a\n' for i in range(n)))
    raise ValueError(family)


def sample(command, env, timeout):
    proc = subprocess.Popen([sys.executable, str(HERE / 'resource_measure.py')] + command,
                            text=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                            env=env, start_new_session=True)
    try:
        stdout, stderr = proc.communicate(timeout=timeout)
    except (subprocess.TimeoutExpired, KeyboardInterrupt):
        import signal
        os.killpg(proc.pid, signal.SIGKILL)
        proc.communicate()
        raise
    if proc.returncode:
        raise RuntimeError(f'{command} exited {proc.returncode}: {stderr[-3000:]}')
    rows = [json.loads(line.split('RESOURCE_SAMPLE ', 1)[1])
            for line in stdout.splitlines() if 'RESOURCE_SAMPLE ' in line]
    if len(rows) != 1:
        raise RuntimeError(f'Expected one sample: {stdout[-3000:]}')
    return rows[0]


def build():
    proc = subprocess.run(['cargo', 'test', '--manifest-path', str(REPO / 'src/Cargo.toml'),
                           '-p', 'pilotdeck-yaml', '--test', 'resource_scaling', '--release',
                           '--offline', '--no-run', '--message-format=json'],
                          capture_output=True, text=True)
    if proc.returncode:
        raise RuntimeError(proc.stderr + proc.stdout)
    for line in proc.stdout.splitlines():
        row = json.loads(line)
        if row.get('reason') == 'compiler-artifact' and row['target']['name'] == 'resource_scaling':
            return row['executable']
    raise RuntimeError('No Rust test binary')


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument('--baseline', required=True)
    parser.add_argument('--node', default='node')
    parser.add_argument('--rust-bin')
    parser.add_argument('--output', default=str(REPO / '_tmp/review-yaml/resources.json'))
    parser.add_argument('--sizes', help='JSON from previous run; reuse Node-only calibration')
    parser.add_argument('--families', nargs='+', choices=FAMILIES, default=FAMILIES)
    parser.add_argument('--repeats', type=int, default=3)
    parser.add_argument('--timeout', type=float, default=120)
    args = parser.parse_args()
    if args.repeats < 1:
        parser.error('repeats must be positive')
    binary = args.rust_bin or build()
    sizes = json.loads(Path(args.sizes).read_text())['sizes'] if args.sizes else {}
    result = {'method': __doc__, 'node': subprocess.check_output([args.node, '--version'], text=True).strip(),
              'baseline': str(Path(args.baseline).resolve()), 'sizes': sizes,
              'repeats': args.repeats, 'host': platform.platform(),
              'baseline_commit': subprocess.check_output(['git', '-C', args.baseline, 'rev-parse', 'HEAD'], text=True).strip(),
              'rows': [], 'failures': []}
    output = Path(args.output)
    output.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix='yaml-resources-') as temp:
        file = Path(temp) / 'input.yaml'
        env = dict(os.environ, PILOTDECK_YAML_RESOURCE_INPUT=str(file))
        node = [args.node, str(HERE / 'resource_worker.mjs'), args.baseline, str(file)]
        rust = [binary, 'resource_worker', '--ignored', '--exact', '--nocapture']
        for family in args.families:
            try:
                if family not in sizes:
                    n = SEEDS[family]
                    for _ in range(16):
                        file.write_text(source(family, n))
                        probe = sample(node, env, args.timeout)
                        print(f'calibrate {family} N={n}: Node {probe["seconds"]:.3f}s', flush=True)
                        if 0.2 <= probe['seconds'] <= 1:
                            break
                        n = max(1, n * 2) if probe['seconds'] < 0.2 else max(1, n // 2)
                    else:
                        raise RuntimeError('Node calibration did not converge to 0.2-1s')
                    sizes[family] = n
                n = sizes[family]
                row = {'family': family, 'n': n, 'samples': {}, 'metrics': {}}
                for scale in [1, 4]:
                    file.write_text(source(family, n * scale))
                    row[f'bytes_{scale}n'] = file.stat().st_size
                    row[f'sha256_{scale}n'] = hashlib.sha256(file.read_bytes()).hexdigest()
                    # Alternate order across repetitions to reduce load/order bias.
                    samples = {'node': [], 'rust': []}
                    for repeat in range(args.repeats):
                        for name in (['node', 'rust'] if repeat % 2 == 0 else ['rust', 'node']):
                            measured = sample(node if name == 'node' else rust, env, args.timeout)
                            samples[name].append(measured)
                            print(f'{family} {scale}N {name}: {measured["seconds"]:.3f}s {measured["rss_bytes"]/1048576:.1f}MiB', flush=True)
                    row['samples'][str(scale)] = samples
                    for name in ['node', 'rust']:
                        row['metrics'][f'{name}_{scale}n'] = {
                            'seconds': statistics.median(s['seconds'] for s in samples[name]),
                            'rss_bytes': max(s['rss_bytes'] for s in samples[name])}
                limits = {}
                m = row['metrics']
                for metric in ['seconds', 'rss_bytes']:
                    for scale in [1, 4]:
                        limits[f'{metric}_rust_node_{scale}n'] = m[f'rust_{scale}n'][metric] / m[f'node_{scale}n'][metric]
                ng = m['node_4n']['seconds'] / m['node_1n']['seconds']
                rg = m['rust_4n']['seconds'] / m['rust_1n']['seconds']
                limits['seconds_node_growth'] = ng
                limits['seconds_rust_growth'] = rg
                limits['seconds_relative_growth'] = rg / ng
                node_delta = m['node_4n']['rss_bytes'] - m['node_1n']['rss_bytes']
                rust_delta = m['rust_4n']['rss_bytes'] - m['rust_1n']['rss_bytes']
                row['rss_increment'] = {
                    'node_bytes': node_delta, 'rust_bytes': rust_delta,
                    'rust_limit_bytes': 1.5 * node_delta,
                    'rust_node_ratio': rust_delta / node_delta if node_delta != 0 else None,
                    'pass': rust_delta <= 1.5 * node_delta}
                row['ratios'] = limits
                row['checks'] = {
                    'same_size_time': all(limits[f'seconds_rust_node_{scale}n'] <= 3 for scale in [1, 4]),
                    'same_size_rss': all(limits[f'rss_bytes_rust_node_{scale}n'] <= 3 for scale in [1, 4]),
                    'time_growth': limits['seconds_relative_growth'] <= 1.5,
                    'rss_increment': row['rss_increment']['pass'],
                    'semantic_counts': all(
                        len({tuple(s[key] for key in ['documents', 'errors', 'warnings', 'conversion_errors'])
                             for samples in row['samples'][str(scale)].values() for s in samples}) == 1
                        for scale in [1, 4])}
                row['pass'] = all(row['checks'].values())
                result['rows'].append(row)
                if not row['pass']:
                    result['failures'].append(family)
                print(f'{family}: {"PASS" if row["pass"] else "FAIL"} {json.dumps(limits)} RSS increment: {json.dumps(row["rss_increment"])}', flush=True)
            except (RuntimeError, subprocess.TimeoutExpired) as error:
                result['failures'].append(family)
                result['rows'].append({'family': family, 'pass': False, 'error': str(error)})
                print(f'{family}: FAIL {error}', flush=True)
            output.write_text(json.dumps(result, indent=2) + '\n')
    print(f'Results: {output}; failures: {result["failures"]}', flush=True)
    return bool(result['failures'])


if __name__ == '__main__':
    raise SystemExit(main())
