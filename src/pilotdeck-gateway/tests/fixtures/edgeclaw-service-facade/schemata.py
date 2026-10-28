#!/usr/bin/env python3
# anchor-selfcheck: unsupported precompiled fixture runner; source anchors are in negative.py
"""#988: compile once, then execute all 24 legacy bindings without editing sources.

CARGO_TARGET_DIR must be supplied. Use --check for read-only anchor preflight,
or NEW_OUTPUT_DIR [mutant ...]. A full run requires Node v22.22.0 on PATH and
PDRS_BASELINE. The existing negative.py remains the HEAD-only comparison arm.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import subprocess
import sys
import time

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
sys.path.insert(0, str(Path(__file__).resolve().parent))
import negative as legacy
from negative_process import run_owned, tree_lock, wait_for_slot
from _shared.mutant_switch import (artifact, clean_environment, execute_suite, publish_summary,
                                    raw_timeout, record_sources, selector_environment,
                                    verify_unchanged)

PREFIX = 'mutant_facade::'
ROOT = legacy.ROOT
BUILD_TIMEOUT = 900
TEST_TIMEOUT = 240


def preflight():
    errors = []
    seen = set()
    for row in legacy.MUTANTS:
        name = row['name']
        if name in seen:
            errors.append(f'{name}: duplicate mutation identity')
        seen.add(name)
        count = row['path'].read_text().count(row['old'])
        if count != 1 or row['old'] == row['new']:
            errors.append(f'{name}: stale anchor ({count} matches) or unchanged replacement')
        # Each legacy binding must have an independently selectable compiled arm.
        # get-nonnumber-maxlines intentionally has two guarded match arms.
        switch = f'::mutants::on("{name}")'
        if switch not in row['path'].read_text() and not (
            row['path'] == legacy.OBSERVE and switch in legacy.TEST_SOURCE.read_text()
        ):
            errors.append(f'{name}: missing compiled switch')
    if errors:
        raise ValueError('\n'.join(errors))


def selected_rows(requested):
    known = {row['name'] for row in legacy.MUTANTS}
    if len(requested) != len(set(requested)) or set(requested) - known:
        raise ValueError('unknown or duplicate mutant name')
    return [row for row in legacy.MUTANTS if not requested or row['name'] in requested]


def run(output, selected, env):
    started = time.monotonic()
    build_seconds = 0.0
    rows = []
    try:
        with tree_lock(ROOT, timeout=900):
            preflight()
            paths = {row['path'] for row in legacy.MUTANTS} | {
                ROOT / 'src/pilotdeck-gateway/src/mutants.rs',
                ROOT / 'src/pilotdeck-gateway/src/lib.rs',
                ROOT / 'src/pilotdeck-gateway/Cargo.toml',
                ROOT / 'src/Cargo.toml', ROOT / 'src/Cargo.lock',
                ROOT / 'src/pilotdeck-gateway/tests/common/baseline.rs',
                legacy.TEST_SOURCE,
                Path(__file__).resolve(), Path(legacy.__file__).resolve(),
            }
            paths.update(Path(__file__).resolve().parent / name
                         for name in ('oracle.json', 'verify.mts', 'generate.mts'))
            paths.update(Path(__file__).resolve().parents[1] / name
                         for name in ('negative_suite.py', 'negative_process.py',
                                     '_shared/mutant_switch.py'))
            hashes = record_sources(ROOT, output, paths)
            gate = wait_for_slot(ROOT / 'scripts/cargo-slots.sh', timeout=900)
            (output / 'build.slots.log').write_text(gate.stdout + gate.stderr)
            command = ['cargo', 'test', '--offline', '--locked',
                       '--manifest-path', str(ROOT / 'src/Cargo.toml'),
                       '-p', 'pilotdeck-gateway', '--lib', '--features', 'pdrs-mutants',
                       '--no-run', '--message-format=json']
            begin = time.monotonic()
            try:
                result = run_owned(command, cwd=ROOT, env=env, timeout=BUILD_TIMEOUT)
                (output / 'build.log').write_text(result.stdout + result.stderr)
                binary = artifact(result)
            except subprocess.TimeoutExpired as error:
                (output / 'build.log').write_text(raw_timeout(error))
                raise
            finally:
                build_seconds = time.monotonic() - begin
            (output / 'binary.json').write_text(json.dumps({
                'path': str(binary), 'sha256': hashlib.sha256(binary.read_bytes()).hexdigest(),
                'command': command,
            }, indent=2) + '\n')
            node_error = None
            if any(row['include_ignored'] for row in selected):
                try:
                    node = run_owned(['node', '--version'], env=env, cwd=ROOT, timeout=30)
                    (output / 'node-version.log').write_text(node.stdout + node.stderr)
                    if node.returncode or node.stdout.strip() != 'v22.22.0':
                        node_error = 'real-TS control requires Node v22.22.0'
                except Exception as error:
                    node_error = f'node preflight: {type(error).__name__}: {error}'
                    (output / 'node-version.log').write_text(node_error + '\n')

            def invoke(label, test, selector, row):
                if row['include_ignored'] and node_error:
                    raise RuntimeError(node_error)
                child_env = selector_environment(env, selector)
                # The two cwd-changing tests are ignored in a full library run.
                # Exact, separate processes also safely run those bindings here.
                command = [str(binary), PREFIX + test, '--exact', '--nocapture', '--include-ignored']
                begin = time.monotonic()
                try:
                    result = run_owned(command, cwd=ROOT / 'src/pilotdeck-gateway',
                                       env=child_env, timeout=TEST_TIMEOUT)
                    (output / (label + '.log')).write_text(result.stdout + result.stderr)
                    (output / (label + '.json')).write_text(json.dumps({
                        'command': command, 'selector': selector,
                        'returncode': result.returncode, 'seconds': time.monotonic() - begin,
                    }, indent=2) + '\n')
                    return result
                except subprocess.TimeoutExpired as error:
                    (output / (label + '.log')).write_text(raw_timeout(error))
                    raise

            def verdict(result, bound):
                return legacy.classify(result, bound, source_file=
                    'pilotdeck-gateway/src/../tests/edgeclaw_service_facade.rs')

            for row in execute_suite(selected, invoke, verdict, PREFIX):
                rows.append(row)
                publish_summary(output, rows, started, build_seconds)
                print(f"{row['name']}: {row['status']}: {row['reason']}", flush=True)
            verify_unchanged(ROOT, hashes)
    except Exception as error:
        reason = f'runner: {type(error).__name__}: {error}'
        for row in rows:
            row.update(status='INVALID', reason=row['reason'] + '; ' + reason)
        for row in selected[len(rows):]:
            rows.append({key: row[key] for key in ('name', 'test', 'anchor')} | {
                'status': 'INVALID', 'reason': reason})
        print(reason, file=sys.stderr)
    publish_summary(output, rows, started, build_seconds)
    print('summary: ' + ', '.join(f'{state}={sum(row["status"] == state for row in rows)}'
                                 for state in ('KILLED', 'SURVIVED', 'INVALID')), flush=True)
    return int(not rows or any(row['status'] != 'KILLED' for row in rows))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--check', action='store_true')
    parser.add_argument('output', nargs='?')
    parser.add_argument('names', nargs='*')
    args = parser.parse_args()
    try:
        selected = selected_rows(args.names)
        preflight()
        if args.check:
            if args.output or args.names:
                raise ValueError('--check does not take output or names')
            print(f'{len(legacy.MUTANTS)} legacy anchors and compiled switches checked')
            return 0
        if not args.output or not os.environ.get('CARGO_TARGET_DIR'):
            raise ValueError('NEW_OUTPUT_DIR and caller CARGO_TARGET_DIR are required')
        output = Path(args.output).resolve()
        output.mkdir(parents=True, exist_ok=False)
        return run(output, selected, clean_environment(os.environ))
    except (ValueError, OSError) as error:
        print(f'preflight: {error}', file=sys.stderr)
        return 2


if __name__ == '__main__':
    sys.exit(main())
