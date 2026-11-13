#!/usr/bin/env python3
"""#1305: --check; [--compile-only] NEW_OUTPUT [mutation ...]. Commit first.

Each mutant compiles before its exact test runs. Every case is reported, then
restoration and all statuses determine the exit code; never bypass HEAD.
"""
from pathlib import Path
import os
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_process import mutating_sources, run_owned, wait_for_slot, write_source_atomic
from negative_selection import claim_dir, missing_target, refuse, refuse_unusable_target, registered
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
    ('omit-sop', SOURCE, '    sop: sop::METHODS,', '    sop: &[],', TEST, '#1305 registry contains every wired method'),
    ('omit-config', SOURCE, '    config: config::METHODS,', '    config: &[],', TEST, '#1305 registry contains every wired method'),
    ('omit-upload', SOURCE, '    upload: upload::METHODS,', '    upload: &[],', TEST, '#1305 registry contains every wired method'),
    # #1627: these rows complete the one-per-family coverage: methods/mod.rs
    # registers 19 families and this table carries 19 omit-* rows. Omitting
    # each family's slice must fail the same registry completeness assertion.
    ('omit-history', SOURCE, '    history: history::METHODS,', '    history: &[],', TEST, '#1305 registry contains every wired method'),
    ('omit-mcp', SOURCE, '    mcp: mcp::METHODS,', '    mcp: &[],', TEST, '#1305 registry contains every wired method'),
    ('omit-tools', SOURCE, '    tools: tools::METHODS,', '    tools: &[],', TEST, '#1305 registry contains every wired method'),
    ('omit-session-dir', SOURCE, '    session_dir: session_dir::METHODS,', '    session_dir: &[],', TEST, '#1305 registry contains every wired method'),
    ('omit-steer', SOURCE, '    steer: steer::METHODS,', '    steer: &[],', TEST, '#1305 registry contains every wired method'),
    ('omit-permission', SOURCE, '    permission: permission::METHODS,', '    permission: &[],', TEST, '#1305 registry contains every wired method'),
    ('omit-user-dialog', SOURCE, '    user_dialog: user_dialog::METHODS,', '    user_dialog: &[],', TEST, '#1305 registry contains every wired method'),
    ('omit-interaction', SOURCE, '    interaction: interaction::METHODS,', '    interaction: &[],', TEST, '#1305 registry contains every wired method'),
    ('omit-always-on', SOURCE, '    always_on: always_on::METHODS,', '    always_on: &[],', TEST, '#1305 registry contains every wired method'),
    ('omit-weixin-login', SOURCE, '    weixin_login: weixin_login::METHODS,', '    weixin_login: &[],', TEST, '#1305 registry contains every wired method'),
    ('omit-cron', SOURCE, '    cron: cron::METHODS,', '    cron: &[],', TEST, '#1305 registry contains every wired method'),
    ('omit-settings', SOURCE, '    settings: settings::METHODS,', '    settings: &[],', TEST, '#1305 registry contains every wired method'),
    ('duplicate-method', SOURCE, '    runs: runs::METHODS,', '    runs: &["run_get", "run_events", "run_reattach", "run_get"],', TEST, '#1305 registry contains no duplicate method'),
    ('management-gate-missing', MANAGEMENT, '                | "snapshot_get"', '                | "snapshot_get_removed"', GATE_TEST, '#1305 management gate matches registered family'),
    ('management-gate-extra', MANAGEMENT, '                | "commands_list"', '                | "commands_list" | "run_get"', GATE_TEST, '#1305 management gate matches registered family'),
    ('management-phase-swap', FAMILY, 'pub(super) const HANDLER: Handler = Handler::BeforeAdmission(|requests, method, params| {\n    Box::pin(requests.management_request(method, params))\n});', 'pub(super) const HANDLER: Handler = Handler::AfterAdmission(|requests, method, params| {\n    Box::pin(async move {\n        requests.management_request(&method, &params).await.expect("#1305 phase mutant recognized method")\n    })\n});', GATE_TEST, '#1305 registry preserves management admission phase'),
]

# #1642 fix-r5 (review sibling P1#2): the table used to be its own
# denominator -- delete a row and the run still printed "{n} mutation
# anchors OK" and read green (#1538/#1666's original defect). The roster is
# the shared negative_selection registered + refuse pair (rc 2 = "never
# started", the exit-code four-way table in specs/negative-controls/
# README.md), the same shape native-management:537,645 and
# native-registry-switch:129,141 carry; import-time validation covers
# --check and the full run alike.
EXPECTED_NAMES = (
    'omit-management',
    'omit-active-turn',
    'omit-runs',
    'omit-metadata',
    'omit-sop',
    'omit-config',
    'omit-upload',
    'omit-history',
    'omit-mcp',
    'omit-tools',
    'omit-session-dir',
    'omit-steer',
    'omit-permission',
    'omit-user-dialog',
    'omit-interaction',
    'omit-always-on',
    'omit-weixin-login',
    'omit-cron',
    'omit-settings',
    'duplicate-method',
    'management-gate-missing',
    'management-gate-extra',
    'management-phase-swap',
)
try:
    registered(mutations, EXPECTED_NAMES)
except ValueError as error:
    refuse(str(error))


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
        refuse('usage: negative_controls.py --check | [--compile-only] NEW_OUTPUT [mutation ...]')
    # #1642 fix-r4 (review P1-B sibling#3, spec specs/negative-controls/README.md
    # exit-code four-way table): every "never started" refusal goes through
    # negative_selection now and lands before the evidence directory is
    # claimed -- a presence-only `raise SystemExit(str)` check read as rc 1
    # ("ran and not green") and let a blank / dot target claim the directory
    # first. Target first (absent / empty / blank / dot), then the caller's
    # selection, then the claim: an existing output directory refuses
    # (claim_dir) instead of the #953 non-empty reuse dying on rc 1.
    if missing_target():
        refuse('#1305 set CARGO_TARGET_DIR to the caller-owned build tree before running')
    refuse_unusable_target()
    env = dict(os.environ)
    for key in ('PDRS_NEGATIVE_RECEIPTS', 'PDRS_NEGATIVE_PARENT_TEST', 'PDRS_MUTANT'):
        env.pop(key, None)
    selected = args[1:]
    repeated = sorted({name for name in selected if selected.count(name) > 1})
    if repeated:
        refuse('#1305 repeated mutation name: ' + ', '.join(repeated))
    unknown = sorted(set(selected) - {case.mutation for case in selected_rows})
    if unknown:
        refuse('#1305 unknown mutation name: ' + ', '.join(unknown))
    if selected:
        selected_rows = [case for case in selected_rows if case.mutation in selected]
    out = claim_dir(args[0])
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
                    write_source_atomic(ROOT, case.path, changed.encode())
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
                    # #1908: one atomic rename; a truncating write_bytes here
                    # has the same SIGKILL half-write window as the mutation
                    # write itself.
                    write_source_atomic(ROOT, case.path, originals[case.path])
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
        # #1642 fix-r3 (review a9 sibling shape, DS r2c#2): a SIGTERM
        # deferred through the restore window re-enters as SystemExit(128+
        # signum) and stays off the exit-code table -- swallowing it used to
        # publish an all-INVALID summary over a restored tree and rewrite
        # 143 to 1 (same handler shape as native-management).
        if isinstance(error, SystemExit):
            raise
        # #1642 fix-r4 (review P1-B, MM concurrency#1 + DS concurrency#1): the
        # enter half of the phase map. A guard that never started (dirty tree,
        # busy lock) is a startup refusal, not a red run: rc 2, no summary, and
        # the claim claim_dir made is withdrawn so the host re-running the same
        # output path is not refused on a leftover directory that never ran
        # (same shape as native-management:1163-1184; rmdir only removes the
        # empty case -- a run that already wrote logs keeps them).
        if phase == 'enter':
            try:
                out.rmdir()
            except OSError:
                pass
            refuse(f'guard refused to start: {error!r}')
        reason = f'source guard: {type(error).__name__}: {error}'
        invalidate_completed(results, reason)
        for case in selected_rows[len(results):]:
            record(invalid(case, reason))
        publish_summary(out, results)
        # #1642 fix-r3 (review P1-2): the phase map was inverted here and in
        # frozen-reinsert-guard -- a guard that failed AFTER the body ran
        # returned 2 while its all-INVALID summary sat published in OUT, and
        # a host reading "rc 2 = never started" dropped real failure
        # evidence. Everything after the enter refusal above is a red run
        # (rc 1).
        return 1
    publish_summary(out, results)
    success = 'COMPILED' if compile_only else 'KILLED'
    return int(not results or any(row['status'] != success for row in results))


if __name__ == '__main__':
    raise SystemExit(main())
