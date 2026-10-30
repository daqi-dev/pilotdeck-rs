#!/usr/bin/env python3
"""#1098 integration-test switch: restore the original shared discovery root.

Compile before running positive/mutant/restored controls. The switch is in the
test TempDir, uses the existing pdrs-mutants feature, and never rewrites source.
No alternate HEAD, shadow repository or private target directory is created.
"""
import argparse
import json
import os
from pathlib import Path
import sys
import time

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from _shared import mutant_switch
from negative_process import run_owned, tree_lock, wait_for_slot

ROOT = Path(__file__).resolve().parents[5]
FIXTURES = ROOT / 'src/pilotdeck-gateway/tests/fixtures'
TESTS = ROOT / 'src/pilotdeck-gateway/tests/edgeclaw_heartbeat_indexer.rs'
switches = [
    ('heartbeat-shared-discovery-root', TESTS,
     'heartbeat_roots_do_not_discover_sibling_repository',
     '#1098 heartbeat discovery must not open sibling repositories'),
]
GUARDED = [
    TESTS, ROOT / 'src/pilotdeck-gateway/src/mutants.rs',
    ROOT / 'src/pilotdeck-gateway/src/runtime/memory/edgeclaw/storage.rs',
    ROOT / 'src/pilotdeck-gateway/src/runtime/memory/edgeclaw/storage/accessors.rs',
    ROOT / 'src/pilotdeck-gateway/tests/common/baseline.rs',
    ROOT / 'src/pilotdeck-gateway/Cargo.toml', ROOT / 'src/Cargo.toml', ROOT / 'src/Cargo.lock',
    FIXTURES / '_shared/mutant_switch.py', FIXTURES / 'negative_suite.py',
    FIXTURES / 'negative_process.py', FIXTURES / 'anchor_selfcheck.py',
    Path(__file__), Path(__file__).with_name('oracle.json'),
    Path(__file__).with_name('generate.mts'), Path(__file__).with_name('README.md'),
]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--check', action='store_true')
    parser.add_argument('output', nargs='?')
    args = parser.parse_args()
    rows = [dict(zip(('name', 'path', 'test', 'anchor'), row)) for row in switches]
    mutant_switch.preflight(rows)
    if args.check:
        if args.output:
            raise ValueError('--check does not take an output directory')
        print(f'{len(rows)} switch anchors and bindings checked')
        return 0
    if not args.output or not os.environ.get('CARGO_TARGET_DIR'):
        raise ValueError('NEW_OUTPUT_DIR and caller CARGO_TARGET_DIR are required')
    output = Path(args.output).resolve()
    output.mkdir(parents=True, exist_ok=False)
    env = mutant_switch.clean_environment(os.environ)
    results = []
    started = time.monotonic()
    build_seconds = 0.0
    try:
        with tree_lock(ROOT, timeout=900):
            hashes = mutant_switch.record_sources(ROOT, output, GUARDED)
            slots = wait_for_slot(ROOT / 'scripts/cargo-slots.sh', timeout=900)
            (output / 'build.slots.log').write_text(slots.stdout + slots.stderr)
            command = ['cargo', 'test', '--offline', '--locked', '--manifest-path',
                       str(ROOT / 'src/Cargo.toml'), '-p', 'pilotdeck-gateway',
                       '--test', 'edgeclaw_heartbeat_indexer', '--features', 'pdrs-mutants',
                       '--no-run', '--message-format=json']
            begin = time.monotonic()
            try:
                build = run_owned(command, cwd=ROOT, env=env, timeout=900)
                (output / 'build.log').write_text(build.stdout + build.stderr)
            except Exception as error:
                (output / 'build.log').write_text(mutant_switch.raw_timeout(error)
                    if hasattr(error, 'stdout') else str(error))
                raise
            build_seconds = time.monotonic() - begin
            if build.returncode:
                raise RuntimeError('mutant did not compile; see build.log')
            binaries = set()
            for line in build.stdout.splitlines():
                item = json.loads(line)
                if (item.get('reason') == 'compiler-artifact'
                        and item.get('target', {}).get('name') == 'edgeclaw_heartbeat_indexer'
                        and item.get('profile', {}).get('test') is True
                        and 'pdrs-mutants' in item.get('features', []) and item.get('executable')):
                    binaries.add(item['executable'])
            if len(binaries) != 1:
                raise RuntimeError(f'expected one integration executable, got {len(binaries)}')
            binary = Path(binaries.pop()).resolve()
            if not binary.is_relative_to(Path(env['CARGO_TARGET_DIR']).resolve()) or not binary.is_file():
                raise RuntimeError('binary outside caller target or missing')
            (output / 'binary.json').write_text(json.dumps({'path': str(binary), 'command': command}) + '\n')

            def invoke(label, test, selector, _row):
                try:
                    result = run_owned([str(binary), test, '--exact', '--nocapture'],
                        cwd=ROOT, env=mutant_switch.selector_environment(env, selector), timeout=120)
                except Exception as error:
                    (output / (label + '.log')).write_text(mutant_switch.raw_timeout(error)
                        if hasattr(error, 'stdout') else str(error))
                    raise
                (output / (label + '.log')).write_text(result.stdout + result.stderr)
                return result

            for row in mutant_switch.execute_suite(rows, invoke):
                results.append(row)
                print(f"{row['name']}: {row['status']}: {row['reason']}", flush=True)
            mutant_switch.verify_unchanged(ROOT, hashes)
            mutant_switch.enforce_denominator(ROOT, Path(__file__), results)
    except BaseException as error:
        reason = f'runner: {type(error).__name__}: {error}'
        for row in results:
            row.update(status='INVALID', reason=row['reason'] + '; ' + reason)
        for row in rows[len(results):]:
            results.append({key: row[key] for key in ('name', 'test', 'anchor')} | {
                'status': 'INVALID', 'reason': reason})
        print(reason, file=sys.stderr)
    mutant_switch.publish_summary(output, results, started, build_seconds)
    print('summary: ' + ', '.join(f'{s}={n}' for s, n in mutant_switch.counts(results).items()))
    return int(not results or any(row['status'] != 'KILLED' for row in results))


if __name__ == '__main__':
    try:
        sys.exit(main())
    except Exception as error:
        print(f'{type(error).__name__}: {error}', file=sys.stderr)
        sys.exit(2)
