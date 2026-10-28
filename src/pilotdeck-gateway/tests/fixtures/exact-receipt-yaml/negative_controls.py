#!/usr/bin/env python3
"""#942: the yaml resource driver's rust worker rerun must carry the libtest receipt.

pilotdeck-yaml has no crate shared with the gateway, so #942 ports the #273
exact-receipt criterion into run_resource_scaling.py (libtest_receipt). This
control proves the port kills: renaming the worker's --exact filter makes the
child binary run zero tests and still exit 0, and the driver must fail the
family at its "#942 resource worker rerun matched no test" error instead of
recording a phantom sample.

The ignored cargo test (resource_scaling) wraps the same Python entry point
but cannot restrict the matrix, so this control drives the driver directly
with one fixed-size family (comments at the seed size, no calibration) and a
single repeat; the receipt logic under mutation is identical. Run after
committing the candidate; pre-commit full runs pass the SHA printed by
"git stash create" as --baseline. Never bypass negative_process's HEAD guard.
"""
import argparse
import difflib
import hashlib
import json
import os
from pathlib import Path
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_process import mutating_sources, run_owned, wait_for_slot

# The only family this control needs: plain comments at the seed size, one of
# the unremarkable passing rows on main. Fixed sizes skip Node calibration.
FAMILY = 'comments'
SIZE = 100000


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('output', type=Path, help='fresh evidence directory')
    parser.add_argument('mutations', nargs='*')
    parser.add_argument('--ts-baseline', required=True,
                        help='read-only TS checkout (Node v22, yaml@2.9.0)')
    parser.add_argument('--baseline', default='HEAD')
    args = parser.parse_args()
    if not os.environ.get('CARGO_TARGET_DIR'):
        parser.error('CARGO_TARGET_DIR must be supplied by the caller')
    root = Path(__file__).resolve().parents[5]
    yaml_source = root / 'src/pilotdeck-yaml/tests/run_resource_scaling.py'
    env = os.environ.copy()
    env['CARGO_PROFILE_DEV_DEBUG'] = 'line-tables-only'
    env['CARGO_PROFILE_TEST_DEBUG'] = 'line-tables-only'
    mutations = [
        ('rust-worker-exact-rename', yaml_source,
         "rust = [binary, 'resource_worker', '--ignored', '--exact', '--nocapture']",
         "rust = [binary, 'resource_worker_renamed', '--ignored', '--exact', '--nocapture']",
         FAMILY,
         '#942 resource worker rerun matched no test'),
    ]
    if args.mutations:
        selected = set(args.mutations)
        if not selected <= {row[0] for row in mutations}:
            parser.error('unknown mutation name')
        mutations = [row for row in mutations if row[0] in selected]
    args.output.mkdir(parents=True, exist_ok=False)

    def save(name, text):
        with (args.output / name).open('x') as stream:
            stream.write(text)

    # One release build of the worker binary before any mutation: the mutated
    # file is the Python driver, which cargo never compiles.
    slots = wait_for_slot(root / 'scripts/cargo-slots.sh', timeout=600, interval=10)
    save('build.slots.log', slots.stdout + slots.stderr)
    build = run_owned([
        'cargo', 'test', '--manifest-path', 'src/Cargo.toml', '-p', 'pilotdeck-yaml',
        '--test', 'resource_scaling', '--release', '--offline', '--no-run',
        '--message-format=json',
    ], cwd=root, env=env, timeout=1800)
    save('build.log', build.stdout + build.stderr)
    binary = None
    for line in build.stdout.splitlines():
        try:
            row = json.loads(line)
        except json.JSONDecodeError:
            continue
        if row.get('reason') == 'compiler-artifact' and row['target']['name'] == 'resource_scaling':
            binary = row['executable']
    if build.returncode or not binary:
        print(f'build failed (rc {build.returncode}); see build.log', file=sys.stderr)
        return 1
    save('binary.txt', f'{binary}\n')
    sizes = args.output / 'sizes.json'
    sizes.write_text(json.dumps({'sizes': {FAMILY: SIZE}}) + '\n')

    def run(label):
        result = run_owned([
            sys.executable, str(yaml_source), '--baseline', str(args.ts_baseline),
            '--rust-bin', binary, '--families', FAMILY, '--repeats', '1',
            '--sizes', str(sizes), '--output', str(args.output / f'{label}.json'),
        ], cwd=root, env=env, timeout=1800)
        log = result.stdout + result.stderr
        save(f'{label}.log', log)
        save(f'{label}.exit', f'{result.returncode}\n')
        return result, log

    def passed(result, log):
        return result.returncode == 0 and f'{FAMILY}: PASS' in log

    def classify(result, assertion, log):
        if passed(result, log):
            return 'SURVIVED', 'driver still passed the family'
        if result.returncode != 0 and assertion in log:
            return 'KILLED', 'driver failed the family at its #942 receipt error'
        return 'INVALID', 'no attributable receipt failure (wrong error or missing family)'

    summary = []
    for name, source, old, new, family, assertion in mutations:
        with mutating_sources(root, [source], baseline=args.baseline) as snapshots:
            original = snapshots[source]
            content = original.decode()
            if content.count(old) != 1:
                save(f'{name}.json', json.dumps(
                    {'name': name, 'status': 'INVALID',
                     'reason': f'anchor count {content.count(old)}'}, indent=2) + '\n')
                summary.append({'name': name, 'status': 'INVALID',
                                'reason': f'anchor count {content.count(old)}'})
                continue
            result, log = run(f'{name}-positive')
            positive = passed(result, log)
            changed = content.replace(old, new, 1)
            save(f'{name}.patch', ''.join(difflib.unified_diff(
                content.splitlines(True), changed.splitlines(True), fromfile=str(source), tofile=str(source))))
            source.write_text(changed)
            result, log = run(f'{name}-negative')
            if positive:
                status, reason = classify(result, assertion, log)
            else:
                status, reason = 'INVALID', 'positive control did not pass the family'
        # Restoration and SHA validation belong to mutating_sources, including on failure.
        assert source.read_bytes() == original, f'{name}: source not restored'
        with mutating_sources(root, [source], baseline=args.baseline):
            result, restored = run(f'{name}-restored')
            if positive and not passed(result, restored):
                status, reason = 'INVALID', reason + '; restored family did not pass'
        summary.append({'name': name, 'status': status, 'reason': reason,
                        'sourceSha256': hashlib.sha256(original).hexdigest()})
        save(f'{name}.json', json.dumps(summary[-1], indent=2) + '\n')
        print(f'{name}: {status}: {reason}', flush=True)
    counts = {state: sum(row['status'] == state for row in summary)
              for state in ('KILLED', 'SURVIVED', 'INVALID')}
    save('summary.json', json.dumps({'controls': summary, 'counts': counts}, indent=2) + '\n')
    print('summary: ' + ', '.join(f'{state}={count}' for state, count in counts.items()), flush=True)
    # Every selected case must be accounted for and killed; never stop at the
    # first failure, and never exit 0 while any case survived or was invalid.
    return int(not summary or any(row['status'] != 'KILLED' for row in summary))


if __name__ == '__main__':
    sys.exit(main())
