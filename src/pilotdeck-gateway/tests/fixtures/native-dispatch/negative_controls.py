#!/usr/bin/env python3
"""#1305: --check; NEW_OUTPUT [mutation ...]. Commit before mutating sources.

Each mutant compiles before its exact test runs. Every case is reported, then
restoration and all statuses determine the exit code; never bypass HEAD.
"""
from pathlib import Path
import os
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_process import mutating_sources, run_owned, wait_for_slot
from negative_suite import Case, classify, passed, publish_summary

ROOT = Path(__file__).resolve().parents[5]
SOURCE = ROOT / 'src/pilotdeck-gateway/src/native_process/methods/mod.rs'
TEST_SOURCE = SOURCE.with_name('tests.rs')
TEST = 'native_process::methods::tests::registry_matches_current_methods'
mutations = [
    ('omit-management', SOURCE, '    management: management::METHODS,', '    management: &[],', TEST, '#1305 registry contains every wired method'),
    ('omit-active-turn', SOURCE, '    active_turn: active_turn::METHODS,', '    active_turn: &[],', TEST, '#1305 registry contains every wired method'),
    ('omit-runs', SOURCE, '    runs: runs::METHODS,', '    runs: &[],', TEST, '#1305 registry contains every wired method'),
    ('omit-metadata', SOURCE, '    metadata: metadata::METHODS,', '    metadata: &[],', TEST, '#1305 registry contains every wired method'),
    ('duplicate-method', SOURCE, '    runs: runs::METHODS,', '    runs: &["run_get", "run_events", "run_reattach", "run_get"],', TEST, '#1305 registry contains no duplicate method'),
]


def mutate(case, original):
    count = original.count(case.old)
    if count != 1 or case.old == case.new:
        raise ValueError(f'#1305 {case.mutation}: stale anchor (expected 1, got {count})')
    return original.replace(case.old, case.new, 1)


def main():
    selected_rows = [Case(*row) for row in mutations]
    if sys.argv[1:] == ['--check']:
        for case in selected_rows:
            mutate(case, case.path.read_text())
        print(f'#1305 {len(selected_rows)} mutation anchors OK')
        return 0
    if len(sys.argv) < 2:
        raise SystemExit('usage: negative_controls.py --check | NEW_OUTPUT [mutation ...]')
    env = dict(os.environ)
    if not env.get('CARGO_TARGET_DIR'):
        raise SystemExit('#1305 CARGO_TARGET_DIR must be supplied by the caller')
    selected = sys.argv[2:]
    if set(selected) - {case.mutation for case in selected_rows}:
        raise SystemExit('#1305 unknown mutation name')
    if selected:
        selected_rows = [case for case in selected_rows if case.mutation in selected]
    out = Path(sys.argv[1]).resolve()
    out.mkdir(parents=True, exist_ok=False)
    env.update(CARGO_PROFILE_DEV_DEBUG='line-tables-only', CARGO_PROFILE_TEST_DEBUG='line-tables-only')
    command = ['cargo', 'test', '--offline', '--manifest-path', 'src/Cargo.toml', '-p', 'pilotdeck-gateway', '--lib']
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
            with mutating_sources(ROOT, [case.path, TEST_SOURCE, Path(__file__).resolve()]) as originals:
                changed = mutate(case, originals[case.path].decode())
                if not passed(run(case.mutation + '-positive', args), case.test):
                    raise RuntimeError('#1305 positive control failed')
                case.path.write_text(changed)
                compiled = run(case.mutation + '-compile', ['--no-run'])
                row['compiled'] = compiled.returncode == 0
                if not row['compiled']:
                    raise RuntimeError('#1305 mutant did not compile')
                row['status'], row['reason'] = classify(run(case.mutation + '-negative', args), case)
            if not passed(run(case.mutation + '-restored', args), case.test):
                raise RuntimeError('#1305 restored positive control failed')
        except Exception as error:
            row.update(status='INVALID', reason=f'{type(error).__name__}: {error}', compiled=False)
        results.append(row)
        print(f"{case.mutation}: {row['status']}: {row['reason']}", flush=True)
    publish_summary(out, results)
    return int(not results or any(row['status'] != 'KILLED' for row in results))


if __name__ == '__main__':
    raise SystemExit(main())
