#!/usr/bin/env python3
"""#1604 require_text / read-order controls; run after commit.

The #1604 tests live in methods/tests.rs; this runner mutates only
production files that are already on HEAD. Until the #1604 tests themselves
are committed, the shared source guard refuses to run (the whole src tree
must equal HEAD), so the coordinator runs this after the commit lands.

Two mutants (taskbook §2.3, PR #1593 review comment #1 item 3):
  - not-a-string-folds-to-domain: the `TrimGuard::NotAString` arm of
    `require_text` folds back into the domain error instead of the boundary's
    `input.<key>?.trim is not a function` TypeError — the shape the steer /
    run family's old `string()` guards used to produce.
  - grant-reads-sessionkey-before-entry: `grant_session_permission` reads
    `sessionKey` before `entry`, so a nullish params throws naming the wrong
    field (TS reads `entry` first, InProcessGateway.ts:2837 →
    GatewayInteractionCoordinator.ts:128).

Usage: negative_controls.py --check | OUT_DIR [mutation ...]
"""
import os
import sys
from pathlib import Path

root = Path(__file__).resolve().parents[5]
sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_suite import Case, run_cases

METHODS_MOD = root / 'src/pilotdeck-gateway/src/native_process/methods/mod.rs'
PERMISSION = root / 'src/pilotdeck-gateway/src/native_process/methods/permission.rs'

TEST = 'native_process::methods::tests::'

mutations = [
    # `require_text`'s NotAString arm must keep the boundary TypeError, not
    # fold back into the caller's domain error (probes h4/h5/h10/h11,
    # `_tmp/20261026-issue-1604/ts-probe/output.txt`).
    ('not-a-string-folds-to-domain', METHODS_MOD,
     '        TrimGuard::NotAString => Err(RequestError::new(format!(\n'
     '            "input.{key}?.trim is not a function"\n'
     '        ))),',
     '        TrimGuard::NotAString => Err(RequestError {\n'
     '            code: code.into(),\n'
     '            message: message.into(),\n'
     '            details: None,\n'
     '            validation: None,\n'
     '        }),',
     TEST + 'hook_async_result_non_string_ids_take_the_optional_chain_type_error',
     '#1604 non-string sessionKey uses the boundary code'),
    # `grant_session_permission` reads `entry` before `sessionKey`: a nullish
    # params throws on the `entry` property read, not the sessionKey one. The
    # read-order flip first breaks the non-canonical sessionKey grants (a
    # number key is matched as the entry and hits the `entry.trim` TypeError),
    # so the kill lands on that `expect`'s marker, before the nullish-params
    # message assertion later in the same test.
    ('grant-reads-sessionkey-before-entry', PERMISSION,
     '                let entry = match super::input_property(&params, "entry")?.as_ref() {',
     '                let entry = match super::input_property(&params, "sessionKey")?.as_ref() {',
     TEST + 'noncanonical_input_forms_match_ts_probes',
     '#1465 grant accepts a non-canonical sessionKey'),
]


def mutate(content, scope, replacements):
    for old, new in replacements:
        if content.count(old) != 1 or old == new:
            raise RuntimeError(f'#1604 expired or ineffective mutation anchor: {content.count(old)} matches')
        content = content.replace(old, new, 1)
    return content


def main():
    if sys.argv[1:] == ['--check']:
        for name, path, old, new, _test, _assertion in mutations:
            mutate(path.read_text(), None, [(old, new)])
        print(f'#1604 {len(mutations)} mutation anchors OK')
        return 0
    if len(sys.argv) < 2:
        raise SystemExit('usage: negative_controls.py --check | OUT [mutation ...]')
    env = dict(os.environ)
    if not env.get('CARGO_TARGET_DIR'):
        raise SystemExit('CARGO_TARGET_DIR must be supplied by the caller')
    # `run_cases` installs the receipt protocol per case (negative_suite.py);
    # anything inherited from a caller shell is stale and would turn a real
    # KILLED into INVALID (same pop list as the sibling fixtures, #1616 r3).
    for key in ('PDRS_MUTANT', 'PDRS_NEGATIVE_RECEIPTS',
                'PDRS_NEGATIVE_PARENT_TEST', 'CARGO_TERM_QUIET'):
        env.pop(key, None)
    selected_names = sys.argv[2:]
    if set(selected_names) - {row[0] for row in mutations}:
        raise SystemExit('unknown mutation name')
    selected = [Case(*row) for row in mutations
                if not selected_names or row[0] in selected_names]
    out = Path(sys.argv[1]).resolve()
    out.mkdir(parents=True, exist_ok=False)
    env.update(CARGO_PROFILE_DEV_DEBUG='line-tables-only',
               CARGO_PROFILE_TEST_DEBUG='line-tables-only')
    return run_cases(root, selected, out, env, mutate, timeout=900, script=__file__)


if __name__ == '__main__':
    raise SystemExit(main())
