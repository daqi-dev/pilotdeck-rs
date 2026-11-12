#!/usr/bin/env python3
"""#2692 fix-r1/fix-r2/fix-r3/fix-r5/fix-r6: --check; [--compile-only] NEW_OUTPUT [mutation ...]. Commit first.

The submit chain keeps TS's two key domains apart (`ts-prodkey-probe.out`):
the composite run key spells `String(sessionKey)` (null → "null", absent →
"undefined", template_key_checked) while the router-shaped maps (admission,
sessions, fence, coordinator, the busy-gate pre-checks) index by the raw value
itself (router_key: strings are identity, every other category tagged
`\0\0raw:…`, so null and "null" are two sessions over one record spelling).
Each mutation restores one shape of the pre-fix collapse and is bound to the
test that must die:

- `submit-owner-uses-template-key`: the owner maps go back to the template
  spelling, so the null-born turn is invisible to the router-domain gate
  (`submit_in_flight`'s admit deadline dies first).
- `router-key-folds-null`: the Null arm returns the string spelling, so
  null and "null" collapse onto one router key again (probe cells b-d).
- `template-key-checked-folds`: the composite-key helper folds a spelling
  error back to "" instead of answering it (the original submit fold).
- `acceptance-error-swallowed`: a key whose ToString throws is accepted
  silently with no result_unknown event (probe cell e's accept-catch shape).
- `owner-conjunction-only`: the model-set busy gate consults only the #1872
  owner conjunction, so the request key never meets its own turn (the
  Grok#2 control cell stops answering SESSION_BUSY).
- `model-set-gate-string-only` / `permission-gate-string-only`: the two busy
  gates go back to string keys only, so a null request never meets its turn
  (probe cells F / permission F).
- `registry-body-spelling` / `submit-raw-spelling`: the record body loses
  the raw JSON null (round-1 rows, re-anchored survivors).
- `registry-skip-reverted`: the fold-band skip is dropped, so a deep-key
  record past the band also skips and the load no longer fails like TS's.
- `registry-skip-silent` (fix-r5): the production signal surface — the
  cumulative counter and the per-read stderr line — is removed, so the
  in-band skip goes back to being observable only through the
  `#[cfg(test)]` accessor (review boundary#3's P0 shape).
- `registry-band-leaf-depth` (fix-r6, review sibling#2): the band criterion
  reverts to the old leaf/node-depth caliber, which under-reports the
  empty-innermost-array shape by exactly one, so the band's first
  out-of-band empty-leaf record (2561) is skipped instead of failing the
  load — under the old caliber this mutation is an identity, so a kill
  proves the array_nesting caliber bites.
- `echo-drops-raw-key`: the read face echoes a fixed null instead of the
  request's own value (probe cell b echo shape).
- `echo-capped-serializer` (fix-r6, review sibling#1): the capped
  provider-body serializer's bound rises past the echo depth, so the
  two-caliber split the deep-echo test pins collapses — the response face
  can no longer be told from the capped one by depth.
- `session-id-uses-router-key` (fix-r3 条1): the SessionContext handoff folds
  the path/persistent-identity domain back onto the router key, so a
  non-string sessionKey turn dies inside the production factory's
  `create_storage` — `std::fs` rejects the raw-value tag's NUL bytes before
  any transcript opens — instead of reaching model resolution, and
  null.jsonl never lands on disk (the production-fixture a1 cell).

Each mutant compiles before its exact test runs. Every case is reported,
then restoration and all statuses determine the exit code; never bypass HEAD.
The execution table is verified against the static EXPECTED_NAMES roster
before anything runs, and every refusal is the shared `negative_selection`
shape (rc 2, `INVALID:` on stderr) — the #2391/#2495 migrated contract.
"""
from pathlib import Path
import os
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_process import (mutating_sources, run_owned, wait_for_slot,
                               write_source_atomic)
from negative_selection import (claim_dir, missing_target, refuse,
                                refuse_unusable_target, registered, verdict_exit)
from negative_suite import (Case, bind_module_file, classify, declares_test,
                            invalidate_completed, passed, publish_summary)

ROOT = Path(__file__).resolve().parents[5]
# The five sources the rows mutate; the assertion messages also live in the
# CONFIG test module and in the run-registry / turn-read test modules that
# carry the bound tests, so those files are scanned for pins too (#2692
# fix-r5, review P3#5: an assertion that lives only in a test file used to
# sit outside the literal-anchor coverage).
SUBMIT = ROOT / 'src/pilotdeck-gateway/src/runtime/turn/submit.rs'
GATES = ROOT / 'src/pilotdeck-gateway/src/native_process/methods/config.rs'
REGISTRY = ROOT / 'src/pilotdeck-gateway/src/storage/run_registry.rs'
JS_VALUE = ROOT / 'src/pilotdeck-gateway/src/js_value.rs'
READ = ROOT / 'src/pilotdeck-gateway/src/runtime/turn/read.rs'
REGISTRY_TESTS = ROOT / 'src/pilotdeck-gateway/src/storage/run_registry_tests.rs'
READ_TESTS = ROOT / 'src/pilotdeck-gateway/src/runtime/turn/read_tests.rs'
SOURCE_TEXTS = (SUBMIT, GATES, REGISTRY, JS_VALUE, READ, REGISTRY_TESTS, READ_TESTS)
# fix-r6 (review boundary#5): --check also proves every bound test still
# exists — a renamed test used to leave the roster's string constant green
# until the host's cargo round. The full module path binds to the one file
# that declares it (negative_suite.bind_module_file), then declares_test
# matches the fn / table / scenario! form with comments and literals blanked.
AUDITED_MODULES = (
    ('native_process::methods::config::tests::', GATES),
    ('js_value::tests::', JS_VALUE),
    ('storage::run_registry_tests::', REGISTRY_TESTS),
    ('runtime::turn::read::tests::', READ_TESTS),
)

CFG = 'native_process::methods::config::tests::'
null_test = (CFG + 'session_model_set_null_key_turn_from_the_submit_chain_'
             'answers_the_ts_cells')
deep_test = CFG + 'deep_array_session_key_submit_is_rejected_without_landing_a_record'
grok2_test = CFG + 'session_model_set_null_request_never_answers_for_a_busy_owner_session'
registry_test = ('storage::run_registry_tests::'
                 'null_session_key_body_keeps_json_null_while_the_key_spells_null')
reopen_test = ('storage::run_registry_tests::'
               'deep_session_key_record_reopens_without_bricking_the_other_records')
signal_test = ('storage::run_registry_tests::'
               'unrekeyable_skips_advances_in_production_on_every_read')
echo_test = ('runtime::turn::read::tests::'
             'read_session_messages_reads_the_router_domain_and_echoes_the_raw_key')
deep_echo_test = ('runtime::turn::read::tests::'
                  'deep_session_key_echo_survives_the_response_serializer')
mutations = [
    # The owner maps (admission here, then every fence/coordinator call below)
    # go back to the template spelling, so the null-born turn parks on "null"
    # and the router-domain lookup of the submit_in_flight helper — and of the
    # busy gates — never sees it.
    ('submit-owner-uses-template-key', SUBMIT,
     '        cleanup.reservation = self.owner.begin_turn(&router_key, &run.run_id);',
     '        cleanup.reservation = self.owner.begin_turn(&run.session_key, &run.run_id);',
     null_test, '-keyed turn was never admitted'),
    # The Null arm returns the string spelling, collapsing null and "null"
    # onto one router key (the pre-fix single-domain shape).
    ('router-key-folds-null', JS_VALUE,
     '        JsValue::Null => tagged(format!("{TAG}null")),',
     '        JsValue::Null => JsString::from("null"),',
     null_test, '-keyed turn was never admitted'),
    # The composite-key helper folds a spelling error back to "" (the
    # original unchecked fold) instead of answering it. fix-r5 made the body
    # an explicit delegation to `property_key` (review P3#4), so the anchor
    # is the delegation.
    ('template-key-checked-folds', JS_VALUE,
     '    property_key(value)\n}',
     '    Ok(to_string_units(value).map(JsString::from_utf16).unwrap_or_default())\n}',
     'js_value::tests::template_key_spells_the_template_literal_form',
     '#2692 a too-deep array is an unreadable key, not a blank one'),
    # The accept-catch shape is dropped: a key whose ToString throws is
    # accepted silently, no result_unknown event, nothing reported.
    ('acceptance-error-swallowed', SUBMIT,
     '            (Err(error), _) | (_, Err(error)) => {\n'
     '                emit_all(send, vec![acceptance_error_event(&run_id, &error)]);\n'
     '                return Ok(());\n'
     '            }',
     '            (Err(error), _) | (_, Err(error)) => {\n'
     '                let _ = (&run_id, &error);\n'
     '                return Ok(());\n'
     '            }',
     deep_test, '#2692 the rejection is a single event'),
    # The busy gate consults only the #1872 owner conjunction: the router-key
    # lookup is gone, so a request key never meets its own turn and the
    # owner's spelling stops answering SESSION_BUSY (the Grok#2 control
    # cell, whichever way it dies: the expect_err or the code assert).
    ('owner-conjunction-only', GATES,
     '        if match &request_key {\n'
     '            Ok(key) => self.owner.has_active_turn(key),\n'
     '            Err(_) => false,\n'
     '        } || string_request\n'
     '            && matches!(&request_key, Ok(key) if trimmed_string_is_non_empty(key))\n'
     '            && owner.is_some_and(|owner| self.owner.has_active_turn(owner))\n'
     '        {',
     '        if string_request\n'
     '            && matches!(&request_key, Ok(key) if trimmed_string_is_non_empty(key))\n'
     '            && owner.is_some_and(|owner| self.owner.has_active_turn(owner))\n'
     '        {',
     grok2_test, ["#2692 control: the owner's spelling meets the busy gate",
                  '#2692 control code']),
    # session_model_set's busy gate goes back to string keys only: a null
    # request skips the gate and answers the sessionKey gate instead.
    ('model-set-gate-string-only', GATES,
     '        let request_session_key = params.get("sessionKey").unwrap_or(&UNDEFINED);\n'
     '        let string_request = matches!(request_session_key, JsValue::String(_));\n'
     '        let request_key = crate::js_value::router_key(request_session_key);\n'
     '        if match &request_key {\n'
     '            Ok(key) => self.owner.has_active_turn(key),\n'
     '            Err(_) => false,\n'
     '        } || string_request\n'
     '            && matches!(&request_key, Ok(key) if trimmed_string_is_non_empty(key))\n'
     '            && owner.is_some_and(|owner| self.owner.has_active_turn(owner))\n'
     '        {',
     '        if let Some(key) = params.get("sessionKey").and_then(JsValue::as_string)\n'
     '            && (self.owner.has_active_turn(key)\n'
     '                || trimmed_string_is_non_empty(key)\n'
     '                    && owner.is_some_and(|owner| self.owner.has_active_turn(owner)))\n'
     '        {',
     null_test, '#2692 cell F code'),
    # set_permission_mode's busy gate goes back to string keys only.
    ('permission-gate-string-only', GATES,
     '        let request_key =\n'
     '            crate::js_value::router_key(params.get("sessionKey").unwrap_or(&UNDEFINED));\n'
     '        if request_key.is_ok_and(|key| self.owner.has_active_turn(&key)) {',
     '        if let Some(JsValue::String(key)) = params.get("sessionKey")\n'
     '            && self.owner.has_active_turn(key)\n'
     '        {',
     null_test, '#2692 permission cell F code'),
    # The record body goes back to the spelling string; JSON null is lost
    # (round-1 row, anchor unchanged).
    ('registry-body-spelling', REGISTRY,
     'record.insert("sessionKey".into(), input.session_key_raw.clone());',
     'record.insert("sessionKey".into(), JsValue::String(input.run.session_key.clone()));',
     registry_test, '#2692 the record body keeps the raw JSON null'),
    # The submitter feeds the raw field the spelling instead of the raw
    # value (round-1 row, anchor unchanged).
    ('submit-raw-spelling', SUBMIT,
     '        let acceptance = RunAcceptInput {\n'
     '            run: run.clone(),\n'
     '            request_material: request_material(&input)?.into(),\n'
     '            session_key_raw,\n'
     '        };',
     '        let acceptance = RunAcceptInput {\n'
     '            run: run.clone(),\n'
     '            request_material: request_material(&input)?.into(),\n'
     '            session_key_raw: JsValue::String(run.session_key.clone()),\n'
     '        };',
     null_test, '#2692 the record body keeps the raw JSON null'),
    # The fold-band skip is dropped entirely, so a deep-key record past the
    # throw band also skips and the reopen stops failing like TS's get.
    # fix-r5 recalibrated the band to the join's own frame count
    # (`array_nesting`, review boundary#1) and added the cumulative-counter
    # increment to the skip arm, so both ride in the anchor.
    ('registry-skip-reverted', REGISTRY,
     '                if [project, session, run_id]\n'
     '                    .into_iter()\n'
     '                    .map(array_nesting)\n'
     '                    .max()\n'
     '                    .is_some_and(|depth| depth > TS_KEY_FOLD_DEPTH)\n'
     '                {\n'
     '                    return Err(error);\n'
     '                }\n'
     '                snapshot.unrekeyable += 1;\n'
     '                UNREKEYABLE_SKIPPED.fetch_add(1, Ordering::Relaxed);\n'
     '                continue;',
     '                snapshot.unrekeyable += 1;\n'
     '                continue;',
     reopen_test, "past V8's fold band and must fail the load"),
    # fix-r5 (review boundary#3, P0): the production signal surface — the
    # cumulative counter increment and the per-read stderr line — is removed,
    # restoring the silent skip the review graded P0 (the only observable
    # left is the #[cfg(test)] per-file accessor). The log block sits right
    # after the records loop, so one span covers both halves of the surface.
    ('registry-skip-silent', REGISTRY,
     '                snapshot.unrekeyable += 1;\n'
     '                UNREKEYABLE_SKIPPED.fetch_add(1, Ordering::Relaxed);\n'
     '                continue;\n'
     '            }\n'
     '        };\n'
     '        snapshot.records.insert(key, record.clone());\n'
     '    }\n'
     '    // #2692 fix-r5 (review boundary#3, P0): the in-band skip used to be\n'
     '    // silent in production — the only observable was a `#[cfg(test)]`\n'
     '    // accessor. One stderr line per read that skipped (plus the cumulative\n'
     '    // counter below) is the registered signal surface (R17): the next\n'
     '    // mutation rewrites the file without the skipped records, so an operator\n'
     '    // sees the drop coming on every read, not after the bytes are gone.\n'
     '    if snapshot.unrekeyable > 0 {\n'
     '        eprintln!(\n'
     '            "run registry {}: skipped {} un-rekeyable record(s); the next persist rewrites the file without them",\n'
     '            path.display(),\n'
     '            snapshot.unrekeyable\n'
     '        );\n'
     '    }\n',
     '                snapshot.unrekeyable += 1;\n'
     '                continue;\n'
     '            }\n'
     '        };\n'
     '        snapshot.records.insert(key, record.clone());\n'
     '    }\n',
     signal_test,
     '#2692 the production counter moved — the skip is not a test-only observable'),
    # fix-r6 (review sibling#2): the band criterion mutates back to the old
    # leaf/node-depth caliber (container_depth), which counts the deepest
    # NODE depth — the empty-innermost-array shape under-reports by exactly
    # one, so the band's first out-of-band empty-leaf record (2561 arrays)
    # counts 2560, lands inside the band, and is skipped instead of failing
    # the load. Under the pre-fix-r5 caliber this mutation is an identity
    # (SURVIVED there), so a kill on the empty-leaf leg proves the
    # array_nesting caliber bites; the scalar leg counts the same under both
    # calibers and keeps passing.
    ('registry-band-leaf-depth', REGISTRY,
     '                    .map(array_nesting)\n',
     '                    .map(|value| {\n'
     '                        let mut max = 0;\n'
     '                        let mut pending = vec![(value, 0usize)];\n'
     '                        while let Some((value, depth)) = pending.pop() {\n'
     '                            max = max.max(depth);\n'
     '                            if let JsValue::Array(items) = value {\n'
     '                                pending.extend(\n'
     '                                    items.iter().map(|item| (item, depth + 1)),\n'
     '                                );\n'
     '                            }\n'
     '                        }\n'
     '                        max\n'
     '                    })\n',
     reopen_test, "past V8's fold band and must fail the load"),
    # The read face echoes a fixed null instead of the request's own value,
    # so a string request's miss stops echoing its own spelling.
    ('echo-drops-raw-key', READ,
     '        Some(raw) => {\n'
     '            if let JsValue::Object(object) = &mut snapshot {\n'
     '                object.insert("sessionKey".into(), raw.clone());\n'
     '            }\n'
     '        }',
     '        Some(raw) => {\n'
     '            let _ = raw;\n'
     '            if let JsValue::Object(object) = &mut snapshot {\n'
     '                object.insert("sessionKey".into(), JsValue::Null);\n'
     '            }\n'
     '        }',
     echo_test, '#2692 the miss echoes the raw request value'),
    # fix-r6 (review sibling#1): the capped provider-body serializer's bound
    # rises past the echo depth, so the deep-key miss echo survives BOTH
    # serializers — the two-caliber split the deep-echo test pins (response
    # face iterative/uncapped vs provider body capped) collapses, and the
    # response face could drift onto the capped one without this row.
    ('echo-capped-serializer', JS_VALUE,
     'pub(crate) const MAX_ARRAY_DEPTH: usize = 2000;',
     'pub(crate) const MAX_ARRAY_DEPTH: usize = 5000;',
     deep_echo_test,
     '#2692 the capped provider-body serializer refuses the same value'),
    # fix-r3 条1: the SessionContext handoff folds the path/identity domain
    # back onto the router key (the fix-r1 single-domain shape), so the
    # null-keyed turn dies inside the production factory's create_storage
    # (std::fs rejects the raw-value tag's NUL bytes before any transcript
    # opens) instead of reaching model resolution, and null.jsonl never
    # lands on disk.
    ('session-id-uses-router-key', SUBMIT,
     '                session_id: run.session_key.clone(),',
     '                session_id: router_key.clone(),',
     CFG + 'real_services_null_session_key_turn_dies_at_model_resolution',
     '#2692 the agent_status message'),
]
# Every bound assertion lives verbatim in the scanned sources; each needs its
# own pin or the completeness rule refuses the run (#2065 shape).
LITERAL_ANCHORS = [
    # config.rs spells the admit-deadline assert as an f-string on the poll
    # key (`"#2692 the {router_key:?}-keyed turn was never admitted"`), so the
    # pin — and the anchor classify matches in the panic payload — is the
    # literal tail.
    (GATES, '-keyed turn was never admitted'),
    (JS_VALUE, '#2692 a too-deep array is an unreadable key, not a blank one'),
    (GATES, '#2692 the rejection is a single event'),
    (GATES, "#2692 control: the owner's spelling meets the busy gate"),
    (GATES, '#2692 control code'),
    (GATES, '#2692 cell F code'),
    (GATES, '#2692 permission cell F code'),
    (GATES, '#2692 the record body keeps the raw JSON null'),
    (GATES, '#2692 the agent_status message'),
    # fix-r5 (review P3#5): the run-registry and turn-read test modules joined
    # SOURCE_TEXTS, so the assertions that live there are pinned where they
    # live — plus the fix-r5 signal row's own assertion.
    (REGISTRY_TESTS, '#2692 the record body keeps the raw JSON null'),
    (REGISTRY_TESTS, "past V8's fold band and must fail the load"),
    (REGISTRY_TESTS,
     '#2692 the production counter moved — the skip is not a test-only observable'),
    (READ_TESTS, '#2692 the miss echoes the raw request value'),
    # fix-r6 (review sibling#1): the deep-echo test's caliber pin.
    (READ_TESTS, '#2692 the capped provider-body serializer refuses the same value'),
]

# The denominator is this literal, never `len(mutations)` (#2391 fix-r1,
# specs/negative-controls/README.md invariant 1): `registered()` compares the
# two in order and refuses on any drift, so a dropped, renamed, duplicated or
# reordered row is a startup refusal (rc 2) instead of a silently shorter run
# that still reports "N/N killed".
EXPECTED_NAMES = (
    'submit-owner-uses-template-key',
    'router-key-folds-null',
    'template-key-checked-folds',
    'acceptance-error-swallowed',
    'owner-conjunction-only',
    'model-set-gate-string-only',
    'permission-gate-string-only',
    'registry-body-spelling',
    'submit-raw-spelling',
    'registry-skip-reverted',
    'registry-skip-silent',
    'registry-band-leaf-depth',
    'echo-drops-raw-key',
    'echo-capped-serializer',
    'session-id-uses-router-key',
)

# #2692 fix-r12 (review MM boundary P1): the rows below share their assertion
# string with a sibling row, so their literal pins stay bound through the
# sibling when the row is dropped from BOTH tables at once -- `registered()`
# passes (both sides of the comparison were edited together, which a real
# deletion does because `registered()` forces alignment) and
# `check_literal_anchors` still sees every pin bound (the sibling asserts the
# same verbatim string), so --check stayed green while one control silently
# stopped being audited. The row pins name the rows themselves and refuse
# any drift that loses one, which no pin-level rule can express: a pin is
# keyed by (file, assertion), not by row identity.
ROW_PINS = (
    'submit-owner-uses-template-key',
    'router-key-folds-null',
    'registry-body-spelling',
    'submit-raw-spelling',
    'registry-skip-reverted',
    'registry-band-leaf-depth',
)


def _assertions(case):
    return case.assertion if isinstance(case.assertion, list) else [case.assertion]


def check_literal_anchors(cases, bound_rows=None):
    """`cases` are validated; a pin may bind to any of `bound_rows`."""
    bound_rows = cases if bound_rows is None else bound_rows
    texts = {}
    for path in SOURCE_TEXTS:
        try:
            texts[path] = path.read_text()
        except OSError as error:
            refuse(f'#2692 scanned source unreadable: {path.name}: {error}')
    for case in cases:
        if case.path not in texts:
            refuse('#2692 row mutates a file outside SOURCE_TEXTS: '
                   f'{case.mutation} -> {case.path.name}')
    for path, anchor in LITERAL_ANCHORS:
        if path not in texts:
            refuse(f'#2692 literal anchor declared outside SOURCE_TEXTS: {path.name}')
        if anchor not in texts[path]:
            refuse(f'#2692 literal anchor not in {path.name}: {anchor!r}')
        if not any(anchor == assertion
                   for case in bound_rows for assertion in _assertions(case)):
            refuse(f'#2692 literal anchor unbound to any row: {anchor!r}')
    pinned = set(LITERAL_ANCHORS)
    for case in cases:
        for assertion in _assertions(case):
            unregistered = [path.name for path, text in texts.items()
                            if assertion in text and (path, assertion) not in pinned]
            if unregistered:
                refuse('#2692 unpinned literal anchor '
                       f'(verbatim in {", ".join(unregistered)}): {assertion!r}')


def mutate(case, original):
    count = original.count(case.old)
    if count != 1 or case.old == case.new:
        raise ValueError(f'#2692 {case.mutation}: stale anchor (expected 1, got {count})')
    return original.replace(case.old, case.new, 1)


def check_row_pins(rows):
    """Refuse a run whose shared-assertion rows lost one to a both-tables edit."""
    names = {case.mutation for case in rows}
    for name in ROW_PINS:
        if name not in names:
            refuse(f'#2692 row pin names a row that is gone: {name}')


def main():
    args = sys.argv[1:]
    compile_only = '--compile-only' in args
    args = [arg for arg in args if arg != '--compile-only']
    every_row = [Case(*row) for row in mutations]
    selected_rows = every_row
    # #2692 fix-r2: every refusal below happens before any mutation, so it is
    # rc 2 ("this run never started", printed by `negative_selection.refuse`)
    # and not rc 1 ("ran, not green") — the four-way exit table in
    # `specs/negative-controls/README.md`.
    # #2692 fix-r12 (review MM boundary P1 / sibling P2): a duplicate name
    # would only surface inside `registered()`'s raw roster-diff list, so the
    # shape gets its own message first (the turn-read / turn-publish
    # duplicate-leg precedent); the row pins then close the both-tables
    # drift hole the shared assertion strings opened.
    names = [row[0] for row in mutations]
    if len(set(names)) != len(names):
        refuse('#2692 the mutation roster has duplicate names')
    try:
        registered(mutations, EXPECTED_NAMES)
    except (ValueError, OSError) as error:
        refuse(f'mutation roster drifted: {error}')
    check_row_pins(every_row)
    # #2495 fix-r2 shape: --check's argv contract is machine-checked — a
    # typo'd `--check some-name` or a silently dropped --compile-only used
    # to return the same green "OK" line as the real roster check without
    # ever reading it.
    if args and args[0] == '--check':
        if compile_only or len(args) > 1:
            refuse('#2692 --check takes no output directory, no mutation names, '
                   'and no --compile-only flag')
        try:
            check_literal_anchors(selected_rows, bound_rows=every_row)
            # fix-r9 (review boundary#2, same fix as native-config): the third
            # count is what the bound-test gate actually passed -- a truncated
            # or deleted loop used to keep printing `len(selected_rows)`.
            declared = 0
            for case in selected_rows:
                mutate(case, case.path.read_text())
                # fix-r6 (review boundary#5): the bound test must still be
                # declared in the one file its module path names.
                tests_path, short = bind_module_file(case.test, AUDITED_MODULES)
                if not declares_test(tests_path.read_text(), short):
                    refuse('#2692 bound test not declared: '
                           f'{case.mutation} -> {case.test}')
                declared += 1
        except (ValueError, OSError) as error:
            refuse(str(error))
        print(f'#2692 {len(selected_rows)} mutation anchors OK; '
              f'{len(LITERAL_ANCHORS)} literal anchors OK; '
              f'{declared} bound tests declared')
        return 0
    if not args:
        refuse('usage: negative_controls.py --check | [--compile-only] NEW_OUTPUT [mutation ...]')
    selected = args[1:]
    unknown = sorted(set(selected) - {case.mutation for case in selected_rows})
    if unknown:
        refuse(f'#2692 unknown mutation names: {unknown}')
    if len(selected) != len(set(selected)):
        repeated = sorted({name for name in selected if selected.count(name) > 1})
        refuse(f'#2692 repeated mutation names: {repeated}')
    if selected:
        selected_rows = [case for case in selected_rows if case.mutation in selected]
    check_literal_anchors(selected_rows, bound_rows=every_row)
    # The target is admitted by the shared rule (absent, empty, blank, dot,
    # root, repo root, inside `src`) *before* the evidence directory exists,
    # so a rejected target cannot leave a directory that reads like a
    # finished run.
    if missing_target():
        refuse('#2692 set CARGO_TARGET_DIR to the caller-owned build tree')
    refuse_unusable_target()
    out = claim_dir(args[0])
    env = dict(os.environ)
    for key in ('PDRS_NEGATIVE_RECEIPTS', 'PDRS_NEGATIVE_PARENT_TEST', 'PDRS_MUTANT'):
        env.pop(key, None)
    env.update(CARGO_PROFILE_DEV_DEBUG='line-tables-only', CARGO_PROFILE_TEST_DEBUG='line-tables-only')
    command = ['cargo', 'test', '--offline', '--manifest-path', 'src/Cargo.toml',
               '-p', 'pilotdeck-gateway', '--lib']
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

    phase = 'enter'
    body_error = None
    try:
        paths = list(dict.fromkeys(case.path for case in selected_rows))
        with mutating_sources(ROOT, [*paths, Path(__file__).resolve()]) as originals:
            phase = 'body'
            try:
                for case in selected_rows:
                    row = invalid(case, '')
                    args = [case.test, '--', '--exact']
                    try:
                        changed = mutate(case, originals[case.path].decode())
                        if not compile_only and not passed(run(case.mutation + '-positive', args), case.test):
                            raise RuntimeError('#2692 positive control failed')
                        write_source_atomic(ROOT, case.path, changed.encode())
                        compiled = run(case.mutation + '-compile', ['--no-run'])
                        row['compiled'] = compiled.returncode == 0
                        if not row['compiled']:
                            raise RuntimeError('#2692 mutant did not compile')
                        if compile_only:
                            row.update(status='COMPILED', reason='mutant cargo test --no-run passed')
                        else:
                            row['status'], row['reason'] = classify(run(case.mutation + '-negative', args), case)
                    except Exception as error:
                        row.update(status='INVALID', reason=f'{type(error).__name__}: {error}')
                    finally:
                        phase = 'restore'
                        write_source_atomic(ROOT, case.path, originals[case.path])
                        if case.path.read_bytes() != originals[case.path]:
                            raise RuntimeError('#2692 source restoration failed')
                        phase = 'body'
                    if not compile_only:
                        try:
                            if not passed(run(case.mutation + '-restored', args), case.test):
                                raise RuntimeError('#2692 restored positive control failed')
                        except Exception as error:
                            row.update(status='INVALID', reason=f'{type(error).__name__}: {error}')
                    record(row)
            except BaseException as error:
                body_error = error
                raise
            phase = 'exit'
    except BaseException as error:
        reason = f'source guard: {type(error).__name__}: {error}'
        invalidate_completed(results, reason)
        for case in selected_rows[len(results):]:
            record(invalid(case, reason))
        publish_summary(out, results)
        if (phase in ('restore', 'exit') or not isinstance(error, Exception)
                or (body_error is not None and error is not body_error)):
            return 2
        return 1
    publish_summary(out, results)
    # The shared exit contract (#2391 fix-r1): zero rows is never 0 (`0/0
    # killed` is not a pass), and every row must be KILLED — COMPILED under
    # --compile-only.
    return verdict_exit(results, key='status',
                        killed='COMPILED' if compile_only else 'KILLED')


if __name__ == '__main__':
    raise SystemExit(main())
