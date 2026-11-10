#!/usr/bin/env python3
"""#2692 submit-chain blank/null sessionKey folding controls. Run after committing the candidate.

Usage: CARGO_TARGET_DIR=... python3 negative_controls.py OUT [mutation ...]
Every selected mutant compiles first; all results are reported before exit.
"""
import argparse
import json
import os
import subprocess
from pathlib import Path
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_process import (CARGO_SLOTS_WAIT_SECONDS, mutating_sources,
                               run_owned, wait_for_slot)
from negative_suite import Case, classify, passed

root = Path(__file__).resolve().parents[5]
submit = root / 'src/pilotdeck-gateway/src/runtime/turn/submit.rs'
gates = root / 'src/pilotdeck-gateway/src/native_process/methods/config.rs'
registry = root / 'src/pilotdeck-gateway/src/storage/run_registry.rs'
js_value = root / 'src/pilotdeck-gateway/src/js_value.rs'
null_test = ('native_process::methods::config::tests::'
              'session_model_set_null_key_turn_from_the_submit_chain_answers_the_ts_cells')
blank_test = ('native_process::methods::config::tests::'
              'session_model_set_blank_key_turn_from_the_submit_chain_answers_the_ts_cells')
registry_test = ('storage::run_registry_tests::'
                 'null_session_key_body_keeps_json_null_while_the_key_spells_null')
cases = [
    # The fix itself: the submit side goes back to folding null to "".
    ('submit-folds-null-to-blank', submit,
     'session_key: crate::js_value::template_key(&session_key_raw),',
     'session_key: string(&session_key_raw),',
     null_test, '#2692 the null-spelling turn was never admitted'),
    # Same fold read through the cross-spelling cell: the second submit of
    # the same runId under the other spelling becomes duplicate replay.
    ('submit-fold-cross-spelling', submit,
     'session_key: crate::js_value::template_key(&session_key_raw),',
     'session_key: string(&session_key_raw),',
     blank_test, '#2692 the null-spelling turn was never admitted'),
    # The record body goes back to the spelling string; JSON null is lost.
    ('registry-body-spelling', registry,
     'record.insert("sessionKey".into(), input.session_key_raw.clone());',
     'record.insert("sessionKey".into(), JsValue::String(input.run.session_key.clone()));',
     registry_test, '#2692 the record body keeps the raw JSON null'),
    # The submitter feeds the raw field the spelling instead of the raw value.
    ('submit-raw-spelling', submit,
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
    # session_model_set's busy gate goes back to string keys only: a null
    # request skips the gate and answers the sessionKey gate instead.
    ('model-set-gate-string-only', gates,
     '        let request_key =\n'
     '            crate::js_value::template_key(params.get("sessionKey").unwrap_or(&UNDEFINED));\n'
     '        if self.owner.has_active_turn(&request_key)\n'
     '            || trimmed_string_is_non_empty(&request_key)\n'
     '                && owner.is_some_and(|owner| self.owner.has_active_turn(owner))\n'
     '        {',
     '        if let Some(key) = params.get("sessionKey").and_then(JsValue::as_string)\n'
     '            && (self.owner.has_active_turn(key)\n'
     '                || trimmed_string_is_non_empty(key)\n'
     '                    && owner.is_some_and(|owner| self.owner.has_active_turn(owner)))\n'
     '        {',
     null_test, '#2692 cell F code'),
    # set_permission_mode's busy gate goes back to string keys only.
    ('permission-gate-string-only', gates,
     '        let request_key =\n'
     '            crate::js_value::template_key(params.get("sessionKey").unwrap_or(&UNDEFINED));\n'
     '        if self.owner.has_active_turn(&request_key) {',
     '        if let Some(JsValue::String(key)) = params.get("sessionKey")\n'
     '            && self.owner.has_active_turn(key)\n'
     '        {',
     null_test, '#2692 permission cell F code'),
    # The helper itself folds non-strings to "" (the original submit fold).
    ('template-key-folds', js_value,
     'pub(crate) fn template_key(value: &JsValue) -> JsString {\n'
     '    to_string_units(value)\n'
     '        .map(JsString::from_utf16)\n'
     '        .unwrap_or_default()\n'
     '}',
     'pub(crate) fn template_key(value: &JsValue) -> JsString {\n'
     '    value.as_string().cloned().unwrap_or_default()\n'
     '}',
     'js_value::tests::template_key_spells_the_template_literal_form', '#2692'),
]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--compile-only', action='store_true')
    parser.add_argument('out', type=Path)
    parser.add_argument('mutations', nargs='*')
    args = parser.parse_args()
    env = os.environ.copy()
    if not env.get('CARGO_TARGET_DIR'):
        parser.error('CARGO_TARGET_DIR must be provided by the caller')
    unknown = set(args.mutations) - {row[0] for row in cases}
    if unknown:
        parser.error(f'unknown mutations: {sorted(unknown)}')
    selected = [row for row in cases if not args.mutations or row[0] in args.mutations]
    args.out.mkdir(parents=True, exist_ok=False)
    command = ['cargo', 'test', '--offline', '--manifest-path', 'src/Cargo.toml',
               '-p', 'pilotdeck-gateway', '--lib']
    results = []

    def run(label, test, compile_only=False):
        try:
            # #1908: the wait budget must exceed the single-sample cap
            slot = wait_for_slot(root / 'scripts/cargo-slots.sh', timeout=CARGO_SLOTS_WAIT_SECONDS)
            (args.out / f'{label}.slots.log').write_text(slot.stdout + slot.stderr)
            suffix = ['--no-run'] if compile_only else [test, '--', '--exact']
            result = run_owned(command + suffix, cwd=root, env=env, timeout=300)
            (args.out / f'{label}.log').write_text(result.stdout + result.stderr)
            return result
        except Exception as error:
            raw = ''
            if isinstance(error, subprocess.TimeoutExpired):
                for value in (error.stdout, error.stderr):
                    raw += value.decode(errors='replace') if isinstance(value, bytes) else value or ''
            with (args.out / f'{label}.log').open('a') as log:
                log.write(raw + f'\n{type(error).__name__}: {error}\n')
            raise

    def record(name, test, assertion, status, reason):
        results.append(dict(mutation=name, test=test, assertion=assertion, status=status, reason=reason))
        print(f'{name}: {status}: {reason}', flush=True)

    try:
        with mutating_sources(root, [submit, gates, registry, js_value]) as snapshots:
            for name, path, old, new, test, assertion in selected:
                status, reason = 'INVALID', 'not run'
                try:
                    original = snapshots[path].decode()
                    if original.count(old) != 1:
                        raise ValueError(f'{name}: expected one source anchor')
                    if not args.compile_only and not passed(run(name + '-positive', test), test):
                        reason = 'bound positive test did not pass'
                    else:
                        path.write_text(original.replace(old, new, 1))
                        if run(name + '-compile', test, compile_only=True).returncode != 0:
                            reason = 'mutant failed to compile'
                        elif args.compile_only:
                            status, reason = 'COMPILED', 'mutant cargo test --no-run passed'
                        else:
                            status, reason = classify(run(name + '-negative', test),
                                Case(name, path, old, new, test, assertion))
                except Exception as error:
                    status, reason = 'INVALID', f'{type(error).__name__}: {error}'
                finally:
                    path.write_bytes(snapshots[path])
                try:
                    if not args.compile_only and not passed(run(name + '-restored', test), test):
                        status, reason = 'INVALID', reason + '; restored bound test did not pass'
                except Exception as error:
                    status, reason = 'INVALID', reason + f'; restored: {error}'
                record(name, test, assertion, status, reason)
    except Exception as error:
        reason = f'source guard: {error}'
        for row in results:
            row.update(status='INVALID', reason=row['reason'] + '; ' + reason)
        for name, _, _, _, test, assertion in selected[len(results):]:
            record(name, test, assertion, 'INVALID', reason)
    counts = {state: sum(row['status'] == state for row in results)
              for state in ('KILLED', 'SURVIVED', 'INVALID', 'COMPILED')}
    (args.out / 'summary.json').write_text(json.dumps(dict(controls=results, counts=counts), indent=2) + '\n')
    print(json.dumps(counts), flush=True)
    success = 'COMPILED' if args.compile_only else 'KILLED'
    return int(not results or any(row['status'] != success for row in results))


if __name__ == '__main__':
    if sys.argv[1:] == ['--check-anchors']:
        stale = []
        for name, path, old, _new, _test, _assertion in cases:
            count = path.read_text().count(old)
            if count != 1:
                stale.append((name, count))
        if stale:
            raise SystemExit(f'STALE ANCHORS: {stale}')
        print(f'{len(cases)} anchors: ok')
        raise SystemExit(0)
    raise SystemExit(main())
