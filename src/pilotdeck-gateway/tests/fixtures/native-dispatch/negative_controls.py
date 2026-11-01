#!/usr/bin/env python3
"""#1305: --check; [--compile-only] NEW_OUTPUT [mutation ...]. Commit first.

Each mutant compiles before its exact test runs. Every case is reported, then
restoration and all statuses determine the exit code; never bypass HEAD.
"""
from pathlib import Path
import os
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_process import mutating_sources, run_owned, wait_for_slot
from negative_suite import Case, classify, invalidate_completed, passed, publish_summary

ROOT = Path(__file__).resolve().parents[5]
SOURCE = ROOT / 'src/pilotdeck-gateway/src/native_process/methods/mod.rs'
TEST_SOURCE = SOURCE.with_name('tests.rs')
FAMILY = SOURCE.with_name('management.rs')
MANAGEMENT = ROOT / 'src/pilotdeck-gateway/src/native_process/management.rs'
GATE_TEST = 'native_process::methods::tests::registry_management_gate_agrees'
TEST = 'native_process::methods::tests::registry_matches_current_methods'
mutations = [
    ('omit-management', SOURCE, '    management: management::METHODS,', '    management: &[],', TEST, '#1305 registry contains every wired method'),
    ('omit-active-turn', SOURCE, '    active_turn: active_turn::METHODS,', '    active_turn: &[],', TEST, '#1305 registry contains every wired method'),
    ('omit-runs', SOURCE, '    runs: runs::METHODS,', '    runs: &[],', TEST, '#1305 registry contains every wired method'),
    ('omit-metadata', SOURCE, '    metadata: metadata::METHODS,', '    metadata: &[],', TEST, '#1305 registry contains every wired method'),
    ('duplicate-method', SOURCE, '    runs: runs::METHODS,', '    runs: &["run_get", "run_events", "run_reattach", "run_get"],', TEST, '#1305 registry contains no duplicate method'),
    ('management-gate-missing', MANAGEMENT, '                | "snapshot_get"', '                | "snapshot_get_removed"', GATE_TEST, '#1305 management gate matches registered family'),
    ('management-gate-extra', MANAGEMENT, '                | "commands_list"', '                | "commands_list" | "run_get"', GATE_TEST, '#1305 management gate matches registered family'),
    ('management-phase-swap', FAMILY, 'pub(super) const HANDLER: Handler = Handler::BeforeAdmission(|requests, method, params| {\n    Box::pin(requests.management_request(method, params))\n});', 'pub(super) const HANDLER: Handler = Handler::AfterAdmission(|requests, method, params| {\n    Box::pin(async move {\n        requests.management_request(&method, &params).await.expect("#1305 phase mutant recognized method")\n    })\n});', GATE_TEST, '#1305 registry preserves management admission phase'),
]


def mutate(case, original):
    count = original.count(case.old)
    if count != 1 or case.old == case.new:
        raise ValueError(f'#1305 {case.mutation}: stale anchor (expected 1, got {count})')
    return original.replace(case.old, case.new, 1)


def main():
    args = sys.argv[1:]
    compile_only = '--compile-only' in args
    args = [arg for arg in args if arg != '--compile-only']
    selected_rows = [Case(*row) for row in mutations]
    if args == ['--check']:
        for case in selected_rows:
            mutate(case, case.path.read_text())
        print(f'#1305 {len(selected_rows)} mutation anchors OK')
        return 0
    if not args:
        raise SystemExit('usage: negative_controls.py --check | [--compile-only] NEW_OUTPUT [mutation ...]')
    env = dict(os.environ)
    if not env.get('CARGO_TARGET_DIR'):
        raise SystemExit('#1305 CARGO_TARGET_DIR must be supplied by the caller')
    for key in ('PDRS_NEGATIVE_RECEIPTS', 'PDRS_NEGATIVE_PARENT_TEST', 'PDRS_MUTANT'):
        env.pop(key, None)
    selected = args[1:]
    if (len(selected) != len(set(selected))
            or set(selected) - {case.mutation for case in selected_rows}):
        raise SystemExit('#1305 unknown/duplicate mutation name')
    if selected:
        selected_rows = [case for case in selected_rows if case.mutation in selected]
    out = Path(args[0]).resolve()
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

    def record(row):
        results.append(row)
        print(f"{row['mutation']}: {row['status']}: {row['reason']}", flush=True)

    def invalid(case, reason):
        return dict(mutation=case.mutation, test=case.test, assertion=case.assertion,
                    status='INVALID', reason=reason, compiled=False)

    # One lease covers the complete batch, including every restored positive.
    # No summary is published until the guard has verified restoration on exit.
    phase = 'enter'
    try:
        paths = list(dict.fromkeys(case.path for case in selected_rows))
        with mutating_sources(ROOT, [*paths, TEST_SOURCE, Path(__file__).resolve()]) as originals:
            phase = 'body'
            for case in selected_rows:
                row = invalid(case, '')
                args = [case.test, '--', '--exact']
                try:
                    changed = mutate(case, originals[case.path].decode())
                    if not compile_only and not passed(run(case.mutation + '-positive', args), case.test):
                        raise RuntimeError('#1305 positive control failed')
                    case.path.write_text(changed)
                    compiled = run(case.mutation + '-compile', ['--no-run'])
                    row['compiled'] = compiled.returncode == 0
                    if not row['compiled']:
                        raise RuntimeError('#1305 mutant did not compile')
                    if compile_only:
                        row.update(status='COMPILED', reason='mutant cargo test --no-run passed')
                    else:
                        row['status'], row['reason'] = classify(run(case.mutation + '-negative', args), case)
                except Exception as error:
                    row.update(status='INVALID', reason=f'{type(error).__name__}: {error}')
                finally:
                    phase = 'restore'
                    case.path.write_bytes(originals[case.path])
                    if case.path.read_bytes() != originals[case.path]:
                        raise RuntimeError('#1305 source restoration failed')
                    phase = 'body'
                if not compile_only:
                    try:
                        if not passed(run(case.mutation + '-restored', args), case.test):
                            raise RuntimeError('#1305 restored positive control failed')
                    except Exception as error:
                        row.update(status='INVALID', reason=f'{type(error).__name__}: {error}')
                record(row)
            phase = 'exit'
    except BaseException as error:
        reason = f'source guard: {type(error).__name__}: {error}'
        invalidate_completed(results, reason)
        for case in selected_rows[len(results):]:
            record(invalid(case, reason))
        publish_summary(out, results)
        return 1 if phase == 'enter' and isinstance(error, Exception) else 2
    publish_summary(out, results)
    success = 'COMPILED' if compile_only else 'KILLED'
    return int(not results or any(row['status'] != success for row in results))


if __name__ == '__main__':
    raise SystemExit(main())
