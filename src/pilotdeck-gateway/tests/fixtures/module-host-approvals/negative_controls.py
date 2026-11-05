#!/usr/bin/env python3
"""#1350 compiled controls: compile every switch once, then run the selected
positive, negative and restored cases with negative_suite verdicts. No source
rewriting, no alternate HEAD and no second target directory. Every switch
mutates exactly one module-host approvals transport or store gate and is
killed by the replay cell that pins that gate's TS bytes or the fix-r1
component test that pins its code (17 switches). Run one sample in the
authoring session; the coordinator runs the complete list after the patch
is committed.
"""
import argparse
import os
from pathlib import Path
import sys
sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from _shared import mutant_switch
ROOT = Path(__file__).resolve().parents[5]
FIXTURES = ROOT / 'src/pilotdeck-gateway/tests/fixtures'
SOURCE = ROOT / 'src/pilotdeck-gateway/src/module_host/approvals.rs'
PENDING = ROOT / 'src/pilotdeck-gateway/src/module_host/approvals/pending.rs'
PREFIX = 'module_host::approvals::tests::'
switches = [
    # bearer gate off: every request is the 401, so the served status cell
    # loses its 200.
    ('approvals-auth-off', SOURCE, 'status_handoff_admin', '#1350 cell status-handoff-admin normalized response'),
    # POST gate off: the GET cell falls through to the body gate's 400.
    ('approvals-method-off', SOURCE, 'method_get', '#1350 cell method-get normalized response'),
    # absent-bridge 503 off: the bare cell is served a 200 instead of the 503.
    ('approvals-bridge-unavailable-off', SOURCE, 'bare_status', '#1350 cell bare-status normalized response'),
    # field allowlist off: the extra field reaches the bridge (and dies at its
    # own 401, not the transport's 400).
    ('approvals-whitelist-off', SOURCE, 'whitelist_extra', '#1350 cell whitelist-extra normalized response'),
    # transport scope gate off: the missing-scope cell reaches the approver
    # gate's 401 instead of the transport's 403 (the bridge's own scope check
    # still rejects a present-but-wrong value).
    ('approvals-scope-off', SOURCE, 'scope_missing', '#1350 cell scope-missing normalized response'),
    # approver-authorization injection off: the stub's echo loses the joined
    # header value.
    ('approvals-approver-inject-off', SOURCE, 'stub_echo_approver', '#1350 cell stub-echo-approver normalized response'),
    # conflict mapping off: the 409 becomes the fallback 502.
    ('approvals-conflict-409-off', SOURCE, 'stub_resume_conflict', '#1350 cell stub-resume-conflict normalized response'),
    # failure default code off: the plain-Error failure body loses its code.
    ('approvals-failure-default-off', SOURCE, 'stub_fail_plain', '#1350 cell stub-fail-plain normalized response'),
    # resume shape gate off: the invalid-source cell is resumed through (the
    # bridge only ever sends source 'human').
    ('approvals-resume-validate-off', SOURCE, 'resume_invalid_source', '#1350 cell resume-invalid-source normalized response'),
    # budget off: the over-budget cell is served through to the approver gate
    # instead of the zero-byte close.
    ('approvals-input-budget-off', SOURCE, 'budget_over', '#1350 cell budget-over normalized response'),
    # budget boundary inclusive: exactly 64 KiB becomes the zero-byte close.
    ('approvals-input-budget-boundary', SOURCE, 'budget_exact', '#1350 cell budget-exact normalized response'),
    # duplicate-receipt replay off: the duplicate cell hits the cleared wait's
    # SOP_NOT_WAITING 502 instead of the stored receipt's 200.
    ('approvals-duplicate-off', PENDING, 'resume_duplicate', '#1350 cell resume-duplicate normalized response'),
    # failure body null fallback off: the undefined code writes the illegal
    # {"code":} frame again (fix-r1 direct test).
    ('approvals-failure-body-null-off', SOURCE, 'failure_body_is_valid_json_for_every_code', '#1350 an undefined code must serialize as null'),
    # not-waiting branch off: the late resume on an active session is
    # relabeled with another state error (fix-r1 direct test).
    ('approvals-not-waiting-off', PENDING, 'late_resume_on_an_active_session_is_not_waiting', '#1350 a resume on an active session must be the SOP_NOT_WAITING 502'),
    # authority gate off: the authority-less store resume is relabeled
    # (fix-r1 direct test).
    ('approvals-auth-required-off', PENDING, 'resume_without_authority_requires_auth', '#1350 the authority-less resume must demand the authority'),
    # expected-revision gate off: the revision-less handoff resume is
    # relabeled (fix-r1 direct test).
    ('approvals-revision-required-off', PENDING, 'handoff_resume_without_expected_revision_requires_it', '#1350 the handoff resume must demand the original revision'),
    # legacy-receipt rejection off: the authorization-less receipt replays
    # as a conflict instead (fix-r1 direct test, fix-r2 store-level).
    ('approvals-legacy-receipt-off', PENDING, 'legacy_receipt_replay_is_rejected', '#1350 the legacy receipt replay must be rejected as the legacy shape'),
]
GUARDED = [SOURCE, PENDING,
    ROOT / 'src/pilotdeck-gateway/src/module_host/approvals/tests.rs',
    ROOT / 'src/pilotdeck-gateway/src/module_host.rs',
    ROOT / 'src/pilotdeck-gateway/src/mutants.rs',
    ROOT / 'src/pilotdeck-gateway/src/lib.rs',
    ROOT / 'src/pilotdeck-gateway/tests/common/baseline.rs',
    ROOT / 'src/pilotdeck-gateway/Cargo.toml', ROOT / 'src/Cargo.toml', ROOT / 'src/Cargo.lock',
    FIXTURES / '_shared/mutant_switch.py', FIXTURES / 'negative_suite.py', FIXTURES / 'negative_process.py',
    Path(__file__), Path(__file__).with_name('oracle.json'),
    Path(__file__).with_name('generate.mts'), Path(__file__).with_name('README.md'),
]

def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--check', action='store_true')
    parser.add_argument('output', nargs='?')
    parser.add_argument('names', nargs='*')
    args = parser.parse_args()
    try:
        rows = [dict(zip(('name', 'path', 'test', 'anchor'), row)) for row in switches]
        chosen = mutant_switch.selected(rows, args.names)
        mutant_switch.preflight(rows)
        if args.check:
            if args.output or args.names:
                raise ValueError('--check does not take output or names')
            print(f'{len(rows)} switch anchors and bindings checked')
            return 0
        if not args.output or not os.environ.get('CARGO_TARGET_DIR'):
            raise ValueError('NEW_OUTPUT_DIR and caller CARGO_TARGET_DIR are required')
        output = Path(args.output).resolve()
        output.mkdir(parents=True, exist_ok=False)
        return mutant_switch.run(ROOT, output, chosen,
            mutant_switch.clean_environment(os.environ), guarded=GUARDED,
            prefix=PREFIX, script=Path(__file__), requested=args.names,
            test_timeout=480)
    except (ValueError, OSError) as error:
        print(f'preflight: {error}', file=sys.stderr)
        return 2

if __name__ == '__main__':
    sys.exit(main())
