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
"""
import os
import sys
from pathlib import Path

root = Path(__file__).resolve().parents[5]
sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_suite import Case, run_cases

USER_DIALOG = root / 'src/pilotdeck-gateway/src/native_process/methods/user_dialog.rs'
PERMISSION = root / 'src/pilotdeck-gateway/src/native_process/methods/permission.rs'
# The shared `!input.<key>?.trim()` guard all four user_dialog methods (and
# permission_list) now route through (#1465); the per-method guards this
# fixture used to mutate were folded into it.
METHODS_MOD = root / 'src/pilotdeck-gateway/src/native_process/methods/mod.rs'
DISPATCH = root / 'src/pilotdeck-gateway/src/dispatch.rs'

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
     'ConsumeResult::NotPending => Ok(crate::metadata::object([(\n'
     '                            "delivered",\n'
     '                            JsValue::Bool(false),\n'
     '                        )])),',
     'ConsumeResult::NotPending => Ok(crate::metadata::object([(\n'
     '                            "delivered",\n'
     '                            JsValue::Bool(true),\n'
     '                        )])),',
     TEST + 'user_dialog_respond_unwired_bus_returns_delivered_false',
     '#1437 respond answered on an unwired bus reports delivered:false'),
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


def mutate(content, scope, replacements):
    for old, new in replacements:
        if content.count(old) != 1 or old == new:
            raise RuntimeError(f'#1437 expired or ineffective mutation anchor: {content.count(old)} matches')
        content = content.replace(old, new, 1)
    return content


def main():
    if sys.argv[1:] == ['--check']:
        for name, path, old, new, _test, _assertion in mutations:
            mutate(path.read_text(), None, [(old, new)])
        print(f'#1437 {len(mutations)} mutation anchors OK')
        return 0
    if len(sys.argv) < 2:
        raise SystemExit('usage: negative_controls.py --check | OUT [mutation ...]')
    env = dict(os.environ)
    if not env.get('CARGO_TARGET_DIR'):
        raise SystemExit('CARGO_TARGET_DIR must be supplied by the caller')
    # `run_cases` installs the receipt protocol per case (negative_suite.py:410);
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
