#!/usr/bin/env python3
"""#1437 permission/user_dialog method-layer controls; run after commit.

The new tests live in methods/tests.rs; this runner mutates only production
files that are already on HEAD. Until the #1437 tests themselves are
committed, the shared source guard refuses to run (the whole src tree must
equal HEAD), so the coordinator runs this after the commit lands.

Usage: negative_controls.py --check | OUT_DIR [mutation ...]
"""
import os
import sys
from pathlib import Path

root = Path(__file__).resolve().parents[5]
sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_suite import Case, run_cases

USER_DIALOG = root / 'src/pilotdeck-gateway/src/native_process/methods/user_dialog.rs'
PERMISSION = root / 'src/pilotdeck-gateway/src/native_process/methods/permission.rs'
DISPATCH = root / 'src/pilotdeck-gateway/src/dispatch.rs'

TEST = 'native_process::methods::tests::'

mutations = [
    # user_dialog_list: sessionKey domain code + dialog projection.
    ('list-domain-message', USER_DIALOG,
     'domain_error("INVALID_USER_DIALOG_LIST", "sessionKey is required.")',
     'domain_error("INVALID_USER_DIALOG_LIST", "sessionKey is required!")',
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
     # The mutated constant is interpolated into the rejection message, so the
     # `below` case fails on its message assertion before the `above` case is
     # reached; either assertion is proof the mutation was caught (#1616 r1).
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
     'let session_key = nonempty_string(&params, "sessionKey").map_err(|_| {\n'
     '                        domain_error(\n'
     '                            "INVALID_USER_DIALOG_CLAIM",',
     'let session_key = nonempty_string(&params, "sessionKey").map_err(|_| {\n'
     '                        domain_error(\n'
     '                            "UNWIRED_USER_DIALOG_CLAIM",',
     TEST + 'user_dialog_claim_release_domain_codes',
     '#1437 claim missing sessionKey domain code'),
    ('release-domain-code', USER_DIALOG,
     'let session_key = nonempty_string(&params, "sessionKey").map_err(|_| {\n'
     '                        domain_error(\n'
     '                            "INVALID_USER_DIALOG_RELEASE",',
     'let session_key = nonempty_string(&params, "sessionKey").map_err(|_| {\n'
     '                        domain_error(\n'
     '                            "UNWIRED_USER_DIALOG_RELEASE",',
     TEST + 'user_dialog_claim_release_domain_codes',
     '#1437 release missing sessionKey domain code'),
    ('respond-domain-code', USER_DIALOG,
     'let session_key = nonempty_string(&params, "sessionKey").map_err(|_| {\n'
     '                        domain_error(\n'
     '                            "INVALID_USER_DIALOG_RESPONSE",',
     'let session_key = nonempty_string(&params, "sessionKey").map_err(|_| {\n'
     '                        domain_error(\n'
     '                            "UNWIRED_USER_DIALOG_RESPONSE",',
     TEST + 'user_dialog_claim_release_domain_codes',
     '#1437 respond missing sessionKey domain code'),
    # release requires leaseId too: missing/null/blank is a domain reject.
    ('release-lease-message', USER_DIALOG,
     '                        None | Some(JsValue::Undefined) | Some(JsValue::Null) => {\n'
     '                            return Err(domain_error(\n'
     '                                "INVALID_USER_DIALOG_RELEASE",\n'
     '                                "sessionKey, requestId, and leaseId are required.",\n'
     '                            ));\n'
     '                        }',
     '                        None | Some(JsValue::Undefined) | Some(JsValue::Null) => {\n'
     '                            return Err(domain_error(\n'
     '                                "INVALID_USER_DIALOG_RELEASE",\n'
     '                                "sessionKey and requestId are required.",\n'
     '                            ));\n'
     '                        }',
     TEST + 'user_dialog_claim_release_domain_codes',
     '#1437 release missing leaseId message'),
    # the shared nonempty_string trim guard: "   " must not pass as a key.
    ('user-dialog-blank-trim', USER_DIALOG,
     '            let text = String::from_utf16_lossy(s.units());\n'
     '            !crate::metadata::js_trim(&text).is_empty()\n'
     '        })\n'
     '        .cloned()\n'
     '        .ok_or_else(|| request_error(format!("{key} must be a non-empty string.")))',
     '            let text = String::from_utf16_lossy(s.units());\n'
     '            true\n'
     '        })\n'
     '        .cloned()\n'
     '        .ok_or_else(|| request_error(format!("{key} must be a non-empty string.")))',
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
    # permission blank sessionKey keeps the domain rejection.
    ('permission-blank-trim', PERMISSION,
     'let text = String::from_utf16_lossy(s.units());\n'
     '                        !crate::metadata::js_trim(&text).is_empty()\n'
     '                    })\n'
     '                    .cloned()\n'
     '                    .ok_or_else(|| {\n'
     '                        // TS: DialogGatewayError("INVALID_PERMISSION_LIST",',
     'let text = String::from_utf16_lossy(s.units());\n'
     '                        true\n'
     '                    })\n'
     '                    .cloned()\n'
     '                    .ok_or_else(|| {\n'
     '                        // TS: DialogGatewayError("INVALID_PERMISSION_LIST",',
     TEST + 'permission_blank_session_key_rejected',
     ['#1437 permission_list rejects a blank sessionKey', '#1437 permission_list blank sessionKey keeps the domain code']),
    # the grant arm's blank-sessionKey trim guard (same source, other branch).
    ('grant-blank-trim', PERMISSION,
     'let text = String::from_utf16_lossy(s.units());\n'
     '                        !crate::metadata::js_trim(&text).is_empty()\n'
     '                    })\n'
     '                    .cloned()\n'
     '                    .ok_or_else(|| {\n'
     '                        // TS grantSessionPermission performs no input validation',
     'let text = String::from_utf16_lossy(s.units());\n'
     '                        true\n'
     '                    })\n'
     '                    .cloned()\n'
     '                    .ok_or_else(|| {\n'
     '                        // TS grantSessionPermission performs no input validation',
     TEST + 'permission_blank_session_key_rejected',
     # A blank key that passes the guard makes `grant` succeed, so the run dies
     # on `expect_err` before the domain-code assertion (#1616 r1).
     ['#1437 grant rejects a blank sessionKey', '#1437 grant blank sessionKey keeps the domain code']),
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
