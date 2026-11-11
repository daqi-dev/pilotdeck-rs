#!/usr/bin/env python3
"""#1833 error-class mutations; require a tagged assertion failure, then restore.

Each mutant breaks one error-class decision from the #1833 fix round (the
`name` a config/binding error carries, the shared check-and-map helper, the
one `[object <tag>]` source, and the reload-diagnostics retention guard). A
mutant is killed only when its target test fails with the issue-tagged
assertion message of the assertion the mutant was built to break; a compile
failure is INVALID, never KILLED.

Run with no other writers/builds in this worktree (the whole `src` tree must
equal HEAD; the runner itself is delivered uncommitted, so the coordinator
runs it after the round's commit). Evidence goes in a new output folder;
original source bytes and hashes are verified after every mutation.
"""
import difflib
import hashlib
import json
import os
from pathlib import Path
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_panic import named_test_failed, named_test_killed
from negative_selection import (baseline_passed, claim_dir, missing_target,
                                refuse, refuse_unusable_target, registered,
                                select, verdict_exit)
from negative_process import (CARGO_SLOTS_WAIT_SECONDS, mutating_sources,
                              run_owned, wait_for_slot)

root = Path(__file__).resolve().parents[5]
# Each independent mutant is applied to the original, never cumulatively.
# `helper-collapses-to-*` share one anchor with different replacements, so
# both kinds of the #1833 B1 helper are pinned separately.
mutations = [
    ('binding-join-typeerror-name', 'src/pilotdeck-gateway/src/config/binding.rs',
     '    // reload diagnostics, and TS propagates the raw TypeError (#1833 A1).\n'
     '    e.name = "TypeError";',
     '    // reload diagnostics, and TS propagates the raw TypeError (#1833 A1).'),
    ('read-error-node-class', 'src/pilotdeck-gateway/src/config/mod.rs',
     '            let mut error = Error::new(code, message);\n'
     '            error.name = "Error";\n'
     '            error',
     '            let mut error = Error::new(code, message);\n'
     '            error'),
    ('sops-parse-error-class', 'src/pilotdeck-gateway/src/config/binding.rs',
     '                let mut error = Error::new(e.code, e.message.clone());\n'
     '                error.name = e.name;',
     '                let mut error = Error::new(e.code, e.message.clone());'),
    ('sops-conversion-error-class', 'src/pilotdeck-gateway/src/config/binding.rs',
     '                    let mut error = Error::new("gateway_request_failed", e.message);\n'
     '                    error.name = e.name;',
     '                    let mut error = Error::new("gateway_request_failed", e.message);'),
    ('helper-collapses-to-notprimitive', 'src/pilotdeck-gateway/src/config/value.rs',
     '    string_coercible(v).map_err(coercion_error)',
     '    string_coercible(v).map_err(|_| '
     'coercion_error(crate::js_value::ToStringError::NotPrimitive))'),
    ('helper-collapses-to-stackoverflow', 'src/pilotdeck-gateway/src/config/value.rs',
     '    string_coercible(v).map_err(coercion_error)',
     '    string_coercible(v).map_err(|_| '
     'coercion_error(crate::js_value::ToStringError::StackOverflow))'),
    ('schemaversion-coercion-check-dropped', 'src/pilotdeck-gateway/src/config/mod.rs',
     '    let version = field(&raw, "schemaVersion");\n'
     '    require_string_coercible(version)?;',
     '    let version = field(&raw, "schemaVersion");'),
    ('language-coercion-check-dropped', 'src/pilotdeck-gateway/src/config/sections.rs',
     '    let language = field(raw, "language");\n'
     '    require_string_coercible(language)?;',
     '    let language = field(raw, "language");'),
    ('object-tag-hardcoded', 'src/pilotdeck-gateway/src/js_value.rs',
     '    object.native_tag().unwrap_or("Object")',
     '    let _ = object;\n'
     '    "Object"'),
    ('parse-cors-object-literal', 'src/pilotdeck-gateway/src/channel/api_server/mod.rs',
     '                crate::js_value::display(item).unwrap_or_default()',
     '                match item {\n'
     '                    JsValue::Object(_) => "[object Object]".to_owned(),\n'
     '                    _ => crate::js_value::display(item).unwrap_or_default(),\n'
     '                }'),
    ('store-reload-guard-broadened', 'src/pilotdeck-gateway/src/config/store.rs',
     '            if error.name == "PilotConfigError" && !error.requires_host {',
     '            if !error.requires_host {'),
    ('js-string-object-literal', 'src/pilotdeck-gateway/src/config/value.rs',
     '        JsValue::Object(o) => format!("[object {}]", crate::js_value::object_tag(o)),',
     '        JsValue::Object(_) => "[object Object]".into(),'),
    ('scalar-units-tag-dropped', 'src/pilotdeck-gateway/src/js_value.rs',
     '            out.extend(object_tag(o).encode_utf16());',
     '            out.extend("Object".encode_utf16());'),
]
# #1666: static denominator. `len(mutations)` is not a denominator: deleting a
# row shortens the run silently, and an emptied table prints `0/0 killed`.
EXPECTED_NAMES = (
    'binding-join-typeerror-name', 'read-error-node-class',
    'sops-parse-error-class', 'sops-conversion-error-class',
    'helper-collapses-to-notprimitive', 'helper-collapses-to-stackoverflow',
    'schemaversion-coercion-check-dropped', 'language-coercion-check-dropped',
    'object-tag-hardcoded', 'parse-cors-object-literal',
    'store-reload-guard-broadened', 'js-string-object-literal',
    'scalar-units-tag-dropped',
)
# The exact test and the issue-tagged assertion message each mutant must break.
TARGETS = {
    'binding-join-typeerror-name': (
        'config::binding::tests::non_string_binding_id_reports_the_path_join_type_error',
        "#1833 path.join's TypeError class must not collapse to PilotConfigError"),
    'read-error-node-class': (
        'config::binding::tests::sops_read_race_reports_the_node_error_class',
        '#1833 the fs error class must not collapse to PilotConfigError'),
    'sops-parse-error-class': (
        'config::binding::tests::sops_yaml_parse_failure_keeps_the_yaml_parse_error_class',
        "#1833 parse()'s doc.errors[0] class must survive apply"),
    'sops-conversion-error-class': (
        'config::binding::tests::sops_alias_conversion_failure_keeps_its_error_class',
        "#1833 toJS's ReferenceError class must survive apply"),
    'helper-collapses-to-notprimitive': (
        'config::tests::require_string_coercible_keeps_both_error_kinds',
        '#1833 StackOverflow must map to RangeError, not a collapsed class'),
    'helper-collapses-to-stackoverflow': (
        'config::tests::require_string_coercible_keeps_both_error_kinds',
        '#1833 NotPrimitive must map to TypeError, not a collapsed class'),
    'schemaversion-coercion-check-dropped': (
        'config::tests::coercion_error_class_survives_both_config_call_sites',
        '#1833 schemaVersion: the coercion error class must survive the load'),
    'language-coercion-check-dropped': (
        'config::tests::coercion_error_class_survives_both_config_call_sites',
        '#1833 alwaysOn.language: a throwing String(value) must fail the load'),
    'object-tag-hardcoded': (
        'channel::api_server::tests::parse_cors_origins_stringifies_entries_through_the_shared_joiner',
        "#1833 entries must stringify like TS's value.map(String)"),
    'parse-cors-object-literal': (
        'channel::api_server::tests::parse_cors_origins_stringifies_entries_through_the_shared_joiner',
        "#1833 entries must stringify like TS's value.map(String)"),
    'store-reload-guard-broadened': (
        'config::store::tests::binding_error_classes_keep_the_retained_reload_diagnostics',
        '#1833 a YAMLParseError reload keeps the retained diagnostics'),
    'js-string-object-literal': (
        'config::tests::config_object_value_keeps_the_collection_tag',
        '#1833 C1: the object arm reads the same tag source as the joiner'),
    'scalar-units-tag-dropped': (
        'config::tests::array_stringification_keeps_the_yaml_collection_tag',
        '#1833 C1: the joiner keeps the collection tag (#1550 修 r6)'),
}
selected = sys.argv[2:]
# #1666: ordinary raises, so `python3 -O` cannot strip these guards.
try:
    registered(mutations, EXPECTED_NAMES)
    # `mutations` stays the frozen literal so `anchor_selfcheck.py` can read it
    # statically; the requested subset is a separate name.
    run_rows = select(mutations, selected, always=())
except ValueError as error:
    refuse(str(error))
env = os.environ.copy()
if missing_target(env):  # #1666: absent, empty or blank, one shared predicate
    refuse('CARGO_TARGET_DIR must be supplied by the caller')
refuse_unusable_target()  # #1666 fix-r9: dot / root / src are unusable too
out = claim_dir(sys.argv[1])
summary = []
failures = []


def run(full):
    result = run_owned(['cargo', 'test', '--manifest-path', 'src/Cargo.toml',
                        '-p', 'pilotdeck-gateway', '--lib', full, '--', '--exact'],
                       cwd=root, env=env, timeout=600)
    return result.returncode, result.stdout + result.stderr


with mutating_sources(root, sorted({root / filename for _, filename, _, _ in run_rows})) as snapshots:
    # #819: an unmutated baseline for every named target must be green first.
    baseline_ok = set()
    for test in dict.fromkeys(TARGETS[name][0] for name, *_ in run_rows):
        slots = wait_for_slot(root / 'scripts/cargo-slots.sh',
                              timeout=CARGO_SLOTS_WAIT_SECONDS)
        (out / f'baseline-{test.replace("::", "-")}.slots.log').write_text(
            slots.stdout + slots.stderr)
        code, log = run(test)
        (out / f'baseline-{test.replace("::", "-")}.log').write_text(log)
        ok = code == 0 and baseline_passed(log, test)
        print(f'baseline {test}: {"PASS" if ok else "INVALID"}', flush=True)
        if ok:
            baseline_ok.add(test)
        else:
            failures.append('baseline-' + test)

    for name, filename, old, new in run_rows:
        path = root / filename
        original = snapshots[path]
        before = hashlib.sha256(original).hexdigest()
        content = original.decode()
        test, marker = TARGETS[name]
        try:
            occurrences = content.count(old)
            if occurrences != 1:
                raise ValueError(
                    f'{name}: anchor occurs {occurrences} times (expected 1)')
            changed = content.replace(old, new, 1)
            (out / f'{name}.patch').write_text(''.join(difflib.unified_diff(
                content.splitlines(True), changed.splitlines(True),
                fromfile=filename, tofile=filename)))
            slots = wait_for_slot(root / 'scripts/cargo-slots.sh',
                                  timeout=CARGO_SLOTS_WAIT_SECONDS)
            (out / f'{name}.slots.log').write_text(slots.stdout + slots.stderr)
            path.write_text(changed)
            code, log = run(test)
            (out / f'{name}.log').write_text(log)
            killed = (test in baseline_ok and code == 101
                      and named_test_failed(log, test)
                      and named_test_killed(log, test, marker)
                      and 'could not compile' not in log)
            failure = next((line.strip() for line in log.splitlines()
                           if 'panicked at' in line or 'assertion' in line), '')
            status = 'KILLED' if killed else (
                'SURVIVED' if code == 0 else 'INVALID')
            summary.append(dict(name=name, test=test, status=status,
                                returncode=code, failure=failure,
                                marker=marker, sourceSha256=before))
            print(f'{name}: {status} rc={code} {failure}', flush=True)
            if status != 'KILLED':
                failures.append(name)
        except Exception as error:
            summary.append(dict(name=name, test=test, status='INVALID',
                                reason=f'{type(error).__name__}: {error}',
                                sourceSha256=before))
            failures.append(name)
        finally:
            path.write_bytes(original)
            restored = hashlib.sha256(path.read_bytes()).hexdigest()
            if restored != before:
                raise RuntimeError(
                    f'{name}: restore failed ({restored} != {before}); the tree is dirty')
            (out / 'summary.json').write_text(
                json.dumps(summary, ensure_ascii=False, indent=2) + '\n')
counts = {state: sum(row['status'] == state for row in summary)
          for state in ('KILLED', 'SURVIVED', 'INVALID')}
print('summary: ' + ', '.join(f'{state}={count}' for state, count in counts.items()), flush=True)
if failures:
    print(f'not killed: {", ".join(failures)}', file=sys.stderr)
killed_rows = sum(1 for row in summary if row['status'] == 'KILLED')
print(f'{killed_rows}/{len(run_rows)} killed; every source restored byte-for-byte', flush=True)
# #1666: a zero-row run is never success (`0/0 killed` used to exit 0).
sys.exit(verdict_exit(summary, key='status'))
