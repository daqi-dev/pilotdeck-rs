#!/usr/bin/env python3
"""#1261 guarded native run()-entry call-site mutation; commit first, never bypass HEAD.

NEW_OUTPUT [mutation ...]. --check validates anchors without changing sources.
Every mutant must compile, kill its exact test, and pass after restoration.
The cargo target is the native_process integration test, not --lib: only the
real CARGO_BIN_EXE child exercises run()'s validate_environment call site.
"""
from pathlib import Path
import difflib
import os
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_process import mutating_sources, run_owned, wait_for_slot
from negative_suite import Case, classify, passed, publish_summary

ROOT = Path(__file__).resolve().parents[5]
ENTRY = ROOT / 'src/pilotdeck-gateway/src/native_process.rs'
TEST_FILE = ROOT / 'src/pilotdeck-gateway/tests/native_process.rs'
TEST = 'native_entry_process_rejects_inherited_authority'
# The omitted call can fail at the child deadline (an accepted authority that
# starts listening), the launch/decode steps, or any of the three key
# assertions; every failure point on the test path carries one of these
# markers, so the deletion mutant is bound to this test's own assertions.
MARKERS = [
    '#1261 native entry child rejection deadline',
    '#1261 native entry child launch',
    '#1261 native entry child stderr UTF8',
    '#1261 native entry rejects inherited PILOTDECK_STORAGE_ROOT',
    '#1261 native entry rejects inherited PILOTDECK_SNAPSHOT_ROOT',
    '#1261 native entry rejects inherited PILOTDECK_MANAGED_CONFIG',
]
mutations = [
    ('run-entry-validate-omitted', ENTRY,
     '    let inherited = capture_environment(std::env::vars_os());\n'
     '    validate_environment(&inherited)?;\n',
     '    let inherited = capture_environment(std::env::vars_os());\n',
     TEST, MARKERS),
]


def mutate(case, text):
    if text.count(case.old) != 1 or case.old == case.new:
        raise ValueError(f'#1261 stale anchor: {case.mutation}: {text.count(case.old)}')
    return text.replace(case.old, case.new, 1)


def main():
    selected_rows = [Case(*row) for row in mutations]
    if sys.argv[1:] == ['--check']:
        for case in selected_rows:
            mutate(case, case.path.read_text())
        print(f'#1261 {len(selected_rows)} mutation anchors OK')
        return 0
    if len(sys.argv) < 2:
        raise SystemExit('usage: negative_controls.py NEW_OUTPUT [mutation ...]')
    env = dict(os.environ)
    if not env.get('CARGO_TARGET_DIR'):
        raise SystemExit('CARGO_TARGET_DIR must be supplied by the caller')
    selected = sys.argv[2:]
    if set(selected) - {case.mutation for case in selected_rows}:
        raise SystemExit('unknown mutation name')
    if selected:
        selected_rows = [case for case in selected_rows if case.mutation in selected]
    out = Path(sys.argv[1]).resolve()
    out.mkdir(parents=True, exist_ok=False)
    env.update(CARGO_PROFILE_DEV_DEBUG='line-tables-only', CARGO_PROFILE_TEST_DEBUG='line-tables-only', LANG='en_US.UTF-8')
    env.pop('LC_ALL', None)
    env.pop('LC_MESSAGES', None)
    command = ['cargo', 'test', '--offline', '--manifest-path', 'src/Cargo.toml', '-p', 'pilotdeck-gateway',
               '--test', 'native_process']
    results = []

    def run(label, args):
        slot = wait_for_slot(ROOT / 'scripts/cargo-slots.sh', timeout=900)
        (out / (label + '.slots.log')).write_text(slot.stdout + slot.stderr)
        result = run_owned(command + args, cwd=ROOT, env=env, timeout=900)
        (out / (label + '.log')).write_text(result.stdout + result.stderr)
        return result

    for case in selected_rows:
        row = dict(mutation=case.mutation, test=case.test, assertion=case.assertion,
                   status='INVALID', reason='', compiled=False)
        args = [case.test, '--', '--exact']
        try:
            # Protect the product call site, the judge and the bound test.
            with mutating_sources(ROOT, [case.path, Path(__file__).resolve(), TEST_FILE]) as originals:
                original = originals[case.path].decode()
                changed = mutate(case, original)
                if not passed(run(case.mutation + '-positive', args), case.test):
                    raise RuntimeError('positive control failed')
                (out / (case.mutation + '.patch')).write_text(''.join(difflib.unified_diff(
                    original.splitlines(True), changed.splitlines(True), fromfile=str(case.path), tofile=str(case.path))))
                case.path.write_text(changed)
                compiled = run(case.mutation + '-compile', ['--no-run'])
                row['compiled'] = compiled.returncode == 0
                if not row['compiled']:
                    raise RuntimeError('mutant did not compile')
                row['status'], row['reason'] = classify(run(case.mutation + '-negative', args), case)
            if not passed(run(case.mutation + '-restored', args), case.test):
                raise RuntimeError('restored positive control failed')
        except Exception as error:
            row.update(status='INVALID', reason=f'{type(error).__name__}: {error}')
        results.append(row)
        print(f"{case.mutation}: {row['status']}: {row['reason']}", flush=True)
    publish_summary(out, results)
    return int(not results or any(row['status'] != 'KILLED' for row in results))


if __name__ == '__main__':
    raise SystemExit(main())
