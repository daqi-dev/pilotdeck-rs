#!/usr/bin/env python3
"""#1437 permission/user_dialog method-layer controls; run after commit.

The new tests live in methods/tests.rs; this runner mutates only production
files that are already on HEAD. Until the #1437 tests themselves are
committed, the shared source guard refuses to run (the whole src tree must
equal HEAD), so the coordinator runs this after the commit lands.

Usage: negative_controls.py --check | OUT_DIR [mutation ...]

Scope note: the two capability-advertisement rows this issue added
(`capability-user_dialog_list`, `capability-permission_list`) live in
`../native-management/negative_controls.py` with the rest of the
capability-name rows, bound to `production_host_management_and_restart`;
running this fixture alone does not cover them (#1616 r3).

The execution table is verified against the static EXPECTED_NAMES roster with
the shared `negative_selection.registered` in every mode (--check, the default
full run, and an explicit subset) before anything runs, so a row dropped from
`mutations` cannot silently shrink the denominator, and a drift is a
never-started refusal (exit 2), not a shorter run that still reads green.
"""
import os
import re
import sys
from pathlib import Path

root = Path(__file__).resolve().parents[5]
sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_selection import (claim_dir, missing_target, refuse,
                                 refuse_unusable_target, registered)
from negative_suite import Case, run_cases

USER_DIALOG = root / 'src/pilotdeck-gateway/src/native_process/methods/user_dialog.rs'
PERMISSION = root / 'src/pilotdeck-gateway/src/native_process/methods/permission.rs'
# The shared `!input.<key>?.trim()` guard all four user_dialog methods (and
# permission_list) now route through (#1465); the per-method guards this
# fixture used to mutate were folded into it.
METHODS_MOD = root / 'src/pilotdeck-gateway/src/native_process/methods/mod.rs'
DISPATCH = root / 'src/pilotdeck-gateway/src/dispatch.rs'
TESTS = root / 'src/pilotdeck-gateway/src/native_process/methods/tests.rs'

TEST = 'native_process::methods::tests::'

mutations = [
    # user_dialog_list: sessionKey domain code + dialog projection.
    ('list-domain-message', USER_DIALOG,
     '                        "INVALID_USER_DIALOG_LIST",\n'
     '                        "sessionKey is required.",',
     '                        "INVALID_USER_DIALOG_LIST",\n'
     '                        "sessionKey is required!",',
     TEST + 'user_dialog_list_validates_session_key_and_projects_dialogs',
     '#1437 user_dialog_list missing sessionKey message'),
    ('list-projection-empty', USER_DIALOG,
     '"dialogs",\n                        JsValue::Array(dialogs),',
     '"dialogs",\n                        JsValue::Array(Vec::new()),',
     TEST + 'user_dialog_list_validates_session_key_and_projects_dialogs',
     '#1437 user_dialog_list projects the pending dialog'),
    # ttlMs lease bounds: inclusive 1_000..=300_000.
    ('ttl-lower-bound', USER_DIALOG,
     'const MIN_LEASE_MS: f64 = 1_000.0;',
     'const MIN_LEASE_MS: f64 = 500.0;',
     TEST + 'user_dialog_claim_ttlms_bounds',
     '#1437 below ttlMs is rejected'),
    ('ttl-upper-bound', USER_DIALOG,
     'const MAX_LEASE_MS: f64 = 300_000.0;',
     'const MAX_LEASE_MS: f64 = 400_000.0;',
     TEST + 'user_dialog_claim_ttlms_bounds',
     # A raised maximum does not admit `below` (the minimum still rejects it),
     # so the `below` case survives its `expect_err` and dies on its message
     # assertion, which interpolates the mutated constant; `above` is the
     # second proof point if the loop order ever changes (#1616 r3).
     ['#1437 below ttlMs message', '#1437 above ttlMs is rejected']),
    ('ttl-fraction-allowed', USER_DIALOG,
     '|| ttl_ms.fract() != 0.0',
     '|| false',
     TEST + 'user_dialog_claim_ttlms_bounds',
     '#1437 fractional ttlMs is rejected'),
    # the non-number ttlMs arm is a separate guard from the range check.
    ('ttl-non-number-message', USER_DIALOG,
     '                        Some(JsValue::Number(n)) => *n,\n'
     '                        _ => {\n'
     '                            return Err(domain_error(\n'
     '                                "INVALID_USER_DIALOG_CLAIM",\n'
     '                                format!(\n'
     '                                    "ttlMs must be a safe integer between {MIN_LEASE_MS} and {MAX_LEASE_MS}."\n'
     '                                ),\n'
     '                            ));\n'
     '                        }\n'
     '                    };',
     '                        Some(JsValue::Number(n)) => *n,\n'
     '                        _ => {\n'
     '                            return Err(domain_error(\n'
     '                                "INVALID_USER_DIALOG_CLAIM",\n'
     '                                format!(\n'
     '                                    "unwired ttlMs must be a safe integer between {MIN_LEASE_MS} and {MAX_LEASE_MS}."\n'
     '                                ),\n'
     '                            ));\n'
     '                        }\n'
     '                    };',
     TEST + 'user_dialog_claim_ttlms_bounds',
     '#1437 non-number ttlMs message'),
    # claim/release/respond sessionKey-required domain codes (wired bus).
    ('claim-domain-code', USER_DIALOG,
     '                        "sessionKey",\n'
     '                        "INVALID_USER_DIALOG_CLAIM",',
     '                        "sessionKey",\n'
     '                        "UNWIRED_USER_DIALOG_CLAIM",',
     TEST + 'user_dialog_claim_release_domain_codes',
     '#1437 claim missing sessionKey domain code'),
    ('release-domain-code', USER_DIALOG,
     '                        "sessionKey",\n'
     '                        "INVALID_USER_DIALOG_RELEASE",',
     '                        "sessionKey",\n'
     '                        "UNWIRED_USER_DIALOG_RELEASE",',
     TEST + 'user_dialog_claim_release_domain_codes',
     '#1437 release missing sessionKey domain code'),
    ('respond-domain-code', USER_DIALOG,
     '                        "sessionKey",\n'
     '                        "INVALID_USER_DIALOG_RESPONSE",',
     '                        "sessionKey",\n'
     '                        "UNWIRED_USER_DIALOG_RESPONSE",',
     TEST + 'user_dialog_claim_release_domain_codes',
     '#1437 respond missing sessionKey domain code'),
    # release requires leaseId too: missing/null/blank is a domain reject.
    ('release-lease-message', USER_DIALOG,
     '                        "leaseId",\n'
     '                        "INVALID_USER_DIALOG_RELEASE",\n'
     '                        "sessionKey, requestId, and leaseId are required.",',
     '                        "leaseId",\n'
     '                        "INVALID_USER_DIALOG_RELEASE",\n'
     '                        "sessionKey and requestId are required.",',
     TEST + 'user_dialog_claim_release_domain_codes',
     '#1437 release missing leaseId message'),
    # the shared trim guard behind every `require_text` call: "   " must not
    # pass as a key.
    ('user-dialog-blank-trim', METHODS_MOD,
     '            if crate::metadata::js_trim(&text).is_empty() {\n'
     '                Ok(TrimGuard::Missing)',
     '            if false {\n'
     '                Ok(TrimGuard::Missing)',
     TEST + 'user_dialog_claim_release_domain_codes',
     # A blank key that passes the guard no longer rejects, so the run dies on
     # `expect_err` before the domain-code assertion.
     ['#1437 claim rejects a blank sessionKey', '#1437 claim blank sessionKey domain code']),
    # claim leaseId: a whitespace-only string must stay rejected.
    ('claim-blank-lease-allowed', USER_DIALOG,
     'if crate::metadata::js_trim(&text).is_empty() {\n'
     '                Err(request_error(format!(\n'
     '                    "{key} must be a non-empty string when provided."',
     'if false {\n'
     '                Err(request_error(format!(\n'
     '                    "{key} must be a non-empty string when provided."',
     TEST + 'user_dialog_claim_release_domain_codes',
     '#1437 claim rejects a whitespace-only leaseId'),
    # respond behavior dispatch: missing result + the answered branch.
    ('respond-missing-result-message', USER_DIALOG,
     'let result_val = params.get("result").ok_or_else(|| {\n'
     '                        domain_error(\n'
     '                            "INVALID_USER_DIALOG_RESPONSE",\n'
     '                            "result.behavior must be answered or cancelled.",\n'
     '                        )\n'
     '                    })?;',
     'let result_val = params.get("result").ok_or_else(|| {\n'
     '                        domain_error(\n'
     '                            "INVALID_USER_DIALOG_RESPONSE",\n'
     '                            "unwired result.behavior must be answered or cancelled.",\n'
     '                        )\n'
     '                    })?;',
     TEST + 'user_dialog_respond_behavior_branches',
     '#1437 respond missing result message'),
    ('behavior-dispatch', USER_DIALOG,
     'if behavior.equals_str("answered") {',
     'if behavior.equals_str("answeredx") {',
     TEST + 'user_dialog_respond_behavior_branches',
     '#1437 an answered behavior delivers'),
    # the unknown-behavior arm must stay a reject, not a silent cancel.
    ('behavior-unknown-cancels', USER_DIALOG,
     '                    } else {\n'
     '                        return Err(domain_error(\n'
     '                            "INVALID_USER_DIALOG_RESPONSE",\n'
     '                            "result.behavior must be answered or cancelled.",\n'
     '                        ));\n'
     '                    };',
     '                    } else {\n'
     '                        DialogAnswer::Cancelled(None)\n'
     '                    };',
     TEST + 'user_dialog_respond_behavior_branches',
     # The mutated arm now returns Ok, so the run dies on `expect_err` before
     # the message assertion it was written to break (#1616 r1).
     ['#1437 respond rejects a unknown behavior', '#1437 respond unknown behavior message']),
    # the cancelled arm must still produce a Cancelled answer.
    ('behavior-cancelled-skipped', USER_DIALOG,
     '} else if behavior.equals_str("cancelled") {',
     '} else if behavior.equals_str("cancelledx") {',
     TEST + 'user_dialog_respond_behavior_branches',
     # `cancelledx` falls into the reject arm, so `.expect` fails first (#1616 r1).
     ['#1437 a cancelled behavior delivers', '#1437 a cancelled behavior reports delivered']),
    # USER_DIALOG_LEASE_REQUIRED on a foreign lease.
    ('lease-required-code', USER_DIALOG,
     'code: "USER_DIALOG_LEASE_REQUIRED".into(),',
     'code: "UNWIRED_USER_DIALOG_LEASE_REQUIRED".into(),',
     TEST + 'user_dialog_respond_lease_required',
     '#1437 respond without a lease domain code'),
    # respond on an unwired bus stays Ok({delivered:false}).
    ('notpending-delivered-true', USER_DIALOG,
     'ConsumeResult::NotPending => {\n'
     '                            pre.deliver();\n'
     '                            outbox.deliver();\n'
     '                            Ok(crate::metadata::object([(\n'
     '                                "delivered",\n'
     '                                JsValue::Bool(false),\n',
     'ConsumeResult::NotPending => {\n'
     '                            pre.deliver();\n'
     '                            outbox.deliver();\n'
     '                            Ok(crate::metadata::object([(\n'
     '                                "delivered",\n'
     '                                JsValue::Bool(true),\n',
     TEST + 'user_dialog_respond_unwired_bus_returns_delivered_false',
     # #1600 made the shared fixture a connected bus, so an `answered`
     # response for an unknown id now stops at the no-snapshot guard and never
     # reaches `ConsumeResult::NotPending`; the `cancelled` case is the one that
     # still exports that branch, and it is the assertion this row kills on.
     '#1437 respond cancelled on an unwired bus reports delivered:false'),
    # the two families stay outside LOCAL_METHODS.
    ('local-dispatch-leak', DISPATCH,
     '"supported_agents",',
     '"supported_agents", "user_dialog_list",',
     TEST + 'permission_user_dialog_methods_stay_outside_local_dispatch',
     '#1437 user_dialog_list must stay outside LOCAL_METHODS'),
    # permission_list blank sessionKey keeps the domain rejection and its code.
    ('permission-list-domain-code', PERMISSION,
     '                    "sessionKey",\n'
     '                    "INVALID_PERMISSION_LIST",',
     '                    "sessionKey",\n'
     '                    "UNWIRED_PERMISSION_LIST",',
     TEST + 'permission_blank_session_key_splits_by_surface',
     ['#1437 permission_list rejects a blank sessionKey', '#1437 permission_list blank sessionKey keeps the domain code']),
    # grant keys the rule map by the raw sessionKey (#1465): only the exact
    # `""` alias is suppressed, so a whitespace key must land in its own bucket
    # and must never reach the sessionless rule set.
    ('grant-whitespace-bucket', PERMISSION,
     '                    .filter(|value| !value.units().is_empty());',
     '                    .filter(|value| value.units().is_empty());',
     TEST + 'permission_blank_session_key_splits_by_surface',
     # Inverting the filter sends the whitespace key to the sessionless bucket,
     # so the own-bucket assertion fires before the alias one.
     ['#1437 grant keeps a whitespace sessionKey in its own bucket',
      '#1437 grant whitespace sessionKey must not alias the sessionless bucket']),
]

# #2572: `mutations` is both the registry and the execution table, so a row
# dropped from the table silently shrank the denominator — `--check` printed a
# smaller count and still said OK, and the run handed `run_cases` only the
# survivors. The denominator is this static roster instead, checked with the
# shared `negative_selection.registered` before anything runs: a dropped,
# renamed, duplicated or reordered row is a never-started refusal (exit 2),
# never a shorter green run (#1666 family, same fix shape as issue-2495).
EXPECTED_TOTAL = 21
EXPECTED_NAMES = (
    'list-domain-message',
    'list-projection-empty',
    'ttl-lower-bound',
    'ttl-upper-bound',
    'ttl-fraction-allowed',
    'ttl-non-number-message',
    'claim-domain-code',
    'release-domain-code',
    'respond-domain-code',
    'release-lease-message',
    'user-dialog-blank-trim',
    'claim-blank-lease-allowed',
    'respond-missing-result-message',
    'behavior-dispatch',
    'behavior-unknown-cancels',
    'behavior-cancelled-skipped',
    'lease-required-code',
    'notpending-delivered-true',
    'local-dispatch-leak',
    'permission-list-domain-code',
    'grant-whitespace-bucket',
)


def check_roster():
    """Every mode enters here first: roster drift is a refusal, never a run."""
    try:
        names = registered(mutations, EXPECTED_NAMES)
    except ValueError as error:
        refuse(str(error))
    if len(names) != EXPECTED_TOTAL:
        refuse(f'#1437 fixed mutation denominator: expected {EXPECTED_TOTAL}, '
               f'found {len(names)}')


def mutate(content, scope, replacements):
    for old, new in replacements:
        if content.count(old) != 1 or old == new:
            raise RuntimeError(f'#1437 expired or ineffective mutation anchor: {content.count(old)} matches')
        content = content.replace(old, new, 1)
    return content


def test_body(test, texts=None):
    """The one top-level definition of the bound test in methods/tests.rs.

    The bound tests are top-level `#[test]` / `#[tokio::test]` functions of
    that file, so a column-zero closing brace is the end of the function and
    a sibling's assertion cannot satisfy the binding (same shape as the
    native-storage-stimulus checker).
    """
    if texts is None:
        texts = {TESTS: TESTS.read_text()}
    pattern = r'^(?:async )?fn ' + re.escape(test.rsplit('::', 1)[-1]) + r'\([^\n]*\).*?^}'
    bodies = re.findall(pattern, texts[TESTS], re.M | re.S)
    return bodies[0] if len(bodies) == 1 else ''


# A quoted literal of the bound test's own body (raw strings and escapes kept
# verbatim; a newline inside a literal is impossible in this file's payloads).
_LITERALS = re.compile(r'"(?:[^"\\]|\\.)*"', re.S)


def _template_covers(template, assertion):
    """True when the assertion is a prefix of some instantiation of template.

    Static chunks must appear in order; a `{placeholder}` absorbs anything
    between them, and the assertion may stop inside a chunk. The tail after
    the last matched chunk must be empty or land inside a trailing
    `{placeholder}` -- the same rule native-management carries (its fix-r5 /
    #3143 fix-r1 hardening), so a drifted assertion with an invented tail or a
    bare `{}` template cannot read as anchored.
    """
    chunks = re.split(r'\{[^{}]*\}', template)
    if not any(chunks):
        return False
    trailing = chunks[-1] == ''
    position = 0
    for index, chunk in enumerate(chunks):
        if not chunk:
            continue
        found = assertion.find(chunk, position)
        if found < 0:
            remainder = assertion[position:]
            if index == 0:
                return bool(remainder) and chunk.startswith(remainder)
            return any(remainder.endswith(chunk[:length])
                       for length in range(1, len(chunk) + 1))
        position = found + len(chunk)
    return trailing or not assertion[position:]


def anchored(body, assertion):
    """True when the assertion (or every alternative in a list) is anchored.

    This fixture's bound tests assert through `format!` templates
    (`"#1437 {label} ttlMs is rejected"` with label=below), so a literal-only
    `f'"{assertion}"' in body` scan (the two argparse siblings' form) would
    report most rows stale on a clean tree. The anchor is the text the kill
    judge requires in the panic payload, so the binding accepts the two
    spellings the table uses: the literal itself (or a substring of a
    literal's static text), and an instantiation of a `{placeholder}` template
    the body carries (native-management's `_anchored` semantics).
    """
    if isinstance(assertion, list):
        return all(anchored(body, one) for one in assertion)
    for literal in _LITERALS.findall(body):
        content = literal[1:-1]
        if assertion in content:
            return True
        if '{' in content and _template_covers(content, assertion):
            return True
    return False


def check_binding_scope():
    fixture = ('fn target() {\n'
               '    panic!("#1437 plain anchor");\n'
               '    panic!("#1437 {label} template anchor");\n'
               '}\n'
               'fn sibling() {\n    panic!("#1437 sibling anchor");\n}\n')
    body = test_body('native_process::methods::tests::target', {TESTS: fixture})
    if '#1437 sibling anchor' in body:
        raise RuntimeError('#1437 binding checker accepted a sibling function')
    if not anchored(body, '#1437 plain anchor'):
        raise RuntimeError('#1437 binding checker lost a literal assertion')
    if not anchored(body, '#1437 below template anchor'):
        raise RuntimeError('#1437 binding checker lost a template instantiation')
    if anchored(body, '#1437 absent anchor'):
        raise RuntimeError('#1437 binding checker anchored a missing assertion')
    if anchored(body, '#1437 plain anchor invented tail'):
        raise RuntimeError('#1437 binding checker accepted a tail-extending anchor')


def main():
    check_binding_scope()
    # #2572: every mode verifies the table against the static roster first, so
    # a dropped row refuses to start instead of shrinking the run.
    check_roster()
    if sys.argv[1:] == ['--check']:
        # #2856 fix-r3 (MM boundary#2, a档): the --check used to verify only
        # the anchors; the two argparse siblings also scan the bound test
        # bodies for the assertion, so a renamed test or a drifted assertion
        # string stayed green here and only surfaced in the host's full round.
        stale = []
        for name, path, old, new, test, assertion in mutations:
            text = path.read_text()
            body = test_body(test)
            if (text.count(old) != 1 or old == new or
                    not body or not anchored(body, assertion)):
                stale.append(name)
        if stale:
            raise SystemExit('#1437 stale anchor/test/assertion bindings: '
                             + ', '.join(stale))
        print(f'#1437 {EXPECTED_TOTAL} registered mutations present '
              f'(EXPECTED_NAMES roster matched); {EXPECTED_TOTAL} unique anchors '
              f'and assertion bindings: OK')
        return 0
    # #2572 fix-r2 (both review families): the exact-match above let
    # `--check NAME` fall through to the run path and create a directory
    # named `--check`; any other placement of --check is a never-started
    # refusal. The name refusals also move ahead of the target check so
    # every entry of this batch reports a typo as "never started" (exit 2)
    # — the two argparse siblings check names before their stale and target
    # guards for the same reason.
    if '--check' in sys.argv[1:]:
        refuse('#1437 --check takes no arguments')
    # #2572 fix-r3 (DS r3 boundary P2): the argv parsing is hand-rolled, so
    # a mistyped first token that merely starts with a dash (`--chec`,
    # `--check=1`) must refuse as an unrecognized flag instead of becoming
    # the OUT directory of a full run. (The argparse siblings abbreviate
    # `--chec` to `--check` by prefix, which is a legitimate `--check`.)
    if sys.argv[1:] and sys.argv[1].startswith('-'):
        refuse('#1437 unrecognized flag: ' + sys.argv[1])
    if len(sys.argv) < 2:
        raise SystemExit('usage: negative_controls.py --check | OUT [mutation ...]')
    selected_names = sys.argv[2:]
    # #2572: a repeated name is a caller-side typo (`a a` for `a b`), not a
    # subset; folding it through the membership test would run the row once
    # and report a green over the smaller scope, the same false green as an
    # unknown name (#1734; native-management r6 fixed the same hole in the
    # same diff as its roster). Both are never-started refusals (exit 2).
    repeated = sorted({name for name in selected_names
                       if selected_names.count(name) > 1})
    if repeated:
        refuse('#1437 repeated mutation name: ' + ', '.join(repeated))
    unknown = sorted(set(selected_names) - {row[0] for row in mutations})
    if unknown:
        refuse('#1437 unknown mutation name: ' + ', '.join(unknown))
    # #2856 fix-r3 (MM boundary#1 ≡ sibling#2): the last two rc 1 edges of the
    # startup contract move to the shared helpers -- an absent/empty/blank/dot
    # CARGO_TARGET_DIR and an existing output directory are never-started
    # refusals (rc 2, no evidence directory), the same shape
    # native-registry-switch carries. `raise SystemExit(str)` exits 1 and a
    # bare `mkdir(exist_ok=False)` dies on FileExistsError, and both read as
    # "it ran and was not green" when nothing had started.
    if missing_target():
        refuse('#1437 set CARGO_TARGET_DIR to the caller-owned build tree before running')
    refuse_unusable_target()
    env = dict(os.environ)
    # `run_cases` runs plain cases with no receipt protocol of its own (the
    # #946 ReceiptRun lives in `matrix_main`, negative_suite.py, not here), so
    # anything inherited from a caller shell is stale config: `record_failure`
    # would panic on it and turn a real KILLED into INVALID (same pop list as
    # the sibling fixtures, #1616 r3).
    for key in ('PDRS_MUTANT', 'PDRS_NEGATIVE_RECEIPTS',
                'PDRS_NEGATIVE_PARENT_TEST', 'CARGO_TERM_QUIET'):
        env.pop(key, None)
    selected = [Case(*row) for row in mutations
                if not selected_names or row[0] in selected_names]
    out = claim_dir(sys.argv[1])
    env.update(CARGO_PROFILE_DEV_DEBUG='line-tables-only',
               CARGO_PROFILE_TEST_DEBUG='line-tables-only')
    return run_cases(root, selected, out, env, mutate, timeout=900, script=__file__)


if __name__ == '__main__':
    raise SystemExit(main())
