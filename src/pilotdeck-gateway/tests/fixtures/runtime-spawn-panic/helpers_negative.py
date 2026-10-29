#!/usr/bin/env python3
"""Shared helper controls. Run after commit; stale anchors fail before mutation."""
import argparse
import json
import os
from pathlib import Path
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_panic import named_test_killed
from negative_process import mutating_sources, run_owned, wait_for_slot

root = Path(__file__).resolve().parents[5]
source = root / 'src/pilotdeck-gateway/src/runtime/cleanup_task.rs'
if not os.environ.get('CARGO_TARGET_DIR'):
    sys.exit('CARGO_TARGET_DIR must be supplied by the caller')
parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('output', type=Path)
parser.add_argument('--only', action='append', default=[])
args = parser.parse_args()
args.output.mkdir(parents=True, exist_ok=False)
payload_test = 'panic_messages_preserve_string_bytes_and_leave_fallback_to_callers'
lock_test = 'recovering_lock_preserves_value_mutability_and_poison'
mutations = [
    ('borrowed-payload-lost', source, '.downcast_ref::<&str>()\n        .copied()',
     '.downcast_ref::<&str>().and(None)', payload_test, 'borrowed payload'),
    ('owned-payload-lost', source, '.or_else(|| payload.downcast_ref::<String>().map(String::as_str))',
     '.or(None)', payload_test, 'owned payload'),
    ('opaque-payload-invented', source, '.or_else(|| payload.downcast_ref::<String>().map(String::as_str))',
     '.or_else(|| payload.downcast_ref::<String>().map(String::as_str)).or(Some("invented"))',
     payload_test, 'opaque payload'),
    ('poison-recovery-lost', source, '.unwrap_or_else(|poisoned| poisoned.into_inner())',
     '.unwrap()', lock_test, 'PoisonError'),
    ('poison-flag-cleared', source,
     "pub(crate) fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {\n    mutex",
     "pub(crate) fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {\n    mutex.clear_poison();\n    mutex",
     lock_test, 'recovery must not clear poisoning'),
]

# Validate every anchor even when selecting one sample. A stale anchor is an
# apparatus error, never a surviving or killed mutant.
for name, path, before, _after, _test, _marker in mutations:
    count = path.read_text().count(before)
    if count != 1:
        sys.exit(f'{name}: stale anchor: expected 1 occurrence, got {count}')
if set(args.only) - {m[0] for m in mutations}:
    sys.exit('unknown mutation name')
selected = [m for m in mutations if not args.only or m[0] in args.only]


def run(name, test):
    slots = wait_for_slot(root / 'scripts/cargo-slots.sh')
    with (args.output / (name + '.slots.log')).open('x') as log:
        log.write(slots.stdout + slots.stderr)
    result = run_owned(['cargo', 'test', '--offline', '--manifest-path', 'src/Cargo.toml',
                        '-p', 'pilotdeck-gateway', '--lib',
                        'runtime::cleanup_task::tests::' + test, '--', '--exact'],
                       cwd=root, env=os.environ.copy(), timeout=600)
    raw = result.stdout + result.stderr
    with (args.output / (name + '.log')).open('x') as log:
        log.write(raw)
    return result.returncode, raw


summary = []
failures = []
for name, path, before, after, test, marker in selected:
    full = 'runtime::cleanup_task::tests::' + test
    status = 'INVALID'
    with mutating_sources(root, [path]) as snapshots:
        original = snapshots[path].decode()
        try:
            assert original.count(before) == 1, (name, 'stale anchor')
            code, raw = run(name + '-positive', test)
            positive_ok = code == 0 and '1 passed' in raw
            path.write_text(original.replace(before, after, 1))
            code, raw = run(name, test)
            killed = (positive_ok and code == 101
                      and named_test_killed(raw, full, marker)
                      and 'could not compile' not in raw)
            status = 'KILLED' if killed else (
                'SURVIVED' if code == 0 else 'INVALID')
        except Exception as error:
            status = 'INVALID'
            print(f'{name}: {type(error).__name__}: {error}', flush=True)
        finally:
            path.write_bytes(snapshots[path])
    with mutating_sources(root, [path]):
        code, raw = run(name + '-restored', test)
        restored = code == 0 and '1 passed' in raw
    if not restored:
        status = 'INVALID'
    summary.append({'name': name, 'status': status, 'restored': restored})
    print(f'{name}: {status}; restored {"PASS" if restored else "FAIL"}', flush=True)
    if status != 'KILLED':
        failures.append(name)
with (args.output / 'summary.json').open('x') as log:
    json.dump(summary, log, indent=2)
counts = {state: sum(row['status'] == state for row in summary)
          for state in ('KILLED', 'SURVIVED', 'INVALID')}
print('summary: ' + ', '.join(f'{state}={count}' for state, count in counts.items()),
      flush=True)
if failures:
    print(f'not killed: {", ".join(failures)}', file=sys.stderr)
    sys.exit(1)
