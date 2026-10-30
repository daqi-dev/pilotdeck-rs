#!/usr/bin/env python3
"""#983 HEAD-only decode-budget controls: --check | [--compile-only] NEW_OUTPUT_DIR [name ...].

Commit the patch before running mutants. CARGO_TARGET_DIR must be supplied by
caller; each mutant compiles, runs a nonce-bound test, restores and rechecks it.
The scripts run against committed sources only (mutating_sources refuses
uncommitted product files), so the coordinator runs them on the host after
landing the patch.
"""
import difflib
import json
import os
from pathlib import Path
import sys

sys.dont_write_bytecode = True
root = Path(__file__).resolve().parents[5]
shared = root / 'src/pilotdeck-gateway/tests/fixtures'
sys.path.insert(0, str(shared))
from negative_process import mutating_sources, run_owned, wait_for_slot
from negative_suite import ReceiptRun, judge, passed

transport = root / 'src/pilotdeck-cli/src/sdk/transport.rs'
main_rs = root / 'src/pilotdeck-cli/src/main.rs'
sessions_list = root / 'src/pilotdeck-cli/tests/sdk_sessions_list_exit.rs'
sessions_run = root / 'src/pilotdeck-cli/tests/sdk_run_exit.rs'
mutations = [
    # take_field's missing-key fallback must stay Undefined, not Null: the
    # undefined-result boundary cell pins the CLI's error message.
    ('take-result-undefined-fallback', transport,
     'fn take_field(mut frame: JsValue, name: &str) -> JsValue {\n    match &mut frame {\n        JsValue::Object(object) => object\n            .insert(name.into(), JsValue::Null)\n            .unwrap_or(JsValue::Undefined),',
     'fn take_field(mut frame: JsValue, name: &str) -> JsValue {\n    match &mut frame {\n        JsValue::Object(object) => object\n            .insert(name.into(), JsValue::Null)\n            .unwrap_or(JsValue::Null),',
     'sessions_list_transport_boundaries', '#983 transport boundary output undefined-result'),
    # The streamed reindentation must keep json_pretty's ": " separator.
    ('pretty-colon-space', main_rs,
     "':' => out.write_all(b\": \")?,",
     "':' => out.write_all(b\":\")?,",
     'sessions_list_transport_boundaries', '#983 transport boundary output nested-projection'),
    # ... and its two-space indentation.
    ('pretty-indent', main_rs,
     'out.write_all(b"  ")?;',
     'out.write_all(b" ")?;',
     'sessions_list_transport_boundaries', '#983 transport boundary output nested-projection'),
    # stream_event must still extract the event payload through take_field.
    ('stream-event-field', transport,
     'return Ok(Some((take_field(frame, "event"), final_frame)));',
     'return Ok(Some((take_field(frame, "final"), final_frame)));',
     'run_matches_ts_matrix', '#1058 output bytes and exit'),
]


def check(row):
    name, path, old, new, test, assertion = row
    count = path.read_text().count(old)
    if count != 1 or old == new or not test or not assertion:
        raise ValueError(f'{name}: stale/invalid anchor: expected=1 actual={count}')


def main():
    args = sys.argv[1:]
    if args == ['--check']:
        errors = []
        for row in mutations:
            try:
                check(row)
            except Exception as error:
                errors.append(str(error))
        for error in errors:
            print('INVALID ' + error)
        print(f'#983 anchors: {len(mutations) - len(errors)}/{len(mutations)} valid')
        return int(bool(errors))
    compile_only = '--compile-only' in args
    args = [arg for arg in args if arg != '--compile-only']
    if not args:
        raise SystemExit('usage: --check | [--compile-only] NEW_OUTPUT_DIR [name ...]')
    selected = [row for row in mutations if not args[1:] or row[0] in args[1:]]
    if not selected or (args[1:] and len(selected) != len(args[1:])):
        raise SystemExit('unknown/duplicate mutation selection')
    env = dict(os.environ)
    if not env.get('CARGO_TARGET_DIR'):
        raise SystemExit('CARGO_TARGET_DIR must be supplied by the caller')
    env.pop(ReceiptRun.ENV, None)
    env.pop(ReceiptRun.PARENT_ENV, None)
    out = Path(args[0]).resolve()
    out.mkdir(parents=True, exist_ok=False)

    def run(label, path, test, run_env, *, compile=False):
        slots = wait_for_slot(root / 'scripts/cargo-slots.sh', timeout=600, interval=15)
        (out / f'{label}.slots.log').write_text(slots.stdout + slots.stderr)
        command = ['cargo', 'test', '--offline', '--manifest-path', 'src/Cargo.toml',
                   '-p', 'pilotdeck-cli', '--test', path.stem]
        command += (['--no-run'] if compile else
                    [test, '--', '--exact', '--show-output', '--test-threads=1'])
        result = run_owned(command, cwd=root, env=run_env, timeout=900)
        (out / f'{label}.stdout').write_text(result.stdout)
        (out / f'{label}.stderr').write_text(result.stderr)
        return result

    guarded = [transport, main_rs, sessions_list, sessions_run, Path(__file__).resolve(),
               root / 'src/pilotdeck-cli/Cargo.toml', root / 'src/Cargo.lock',
               shared / 'negative_process.py', shared / 'negative_suite.py']
    results = []
    for row in selected:
        name, path, old, new, test, assertion = row
        item = dict(name=name, test=test, status='INVALID', reason='not run', compiled=False)
        try:
            check(row)
            with mutating_sources(root, guarded) as originals:
                original = originals[path].decode()
                try:
                    if not compile_only and not passed(run(name + '-positive', path, test, env), test):
                        raise RuntimeError('positive bound test failed')
                    changed = original.replace(old, new, 1)
                    (out / f'{name}.patch').write_text(''.join(difflib.unified_diff(
                        original.splitlines(True), changed.splitlines(True),
                        fromfile=str(path.relative_to(root)), tofile=str(path.relative_to(root)))))
                    path.write_text(changed)
                    if run(name + '-compile', path, test, env, compile=True).returncode != 0:
                        raise RuntimeError('mutant compile failed; not evidence')
                    item['compiled'] = True
                    if compile_only:
                        item.update(status='COMPILED', reason='mutant no-run passed')
                    else:
                        receipts = ReceiptRun(out, name + '-negative', [(test, assertion)])
                        result = run(name + '-negative', path, test, receipts.environment(env))
                        killed, reason, _ = judge(result, '', [(test, assertion)], receipts)
                        item.update(status='KILLED' if killed else
                                    ('SURVIVED' if passed(result, test) else 'INVALID'), reason=reason)
                finally:
                    # Only restore bytes supplied by the official HEAD guard.
                    path.write_bytes(originals[path])
                    if path.read_bytes() != originals[path]:
                        raise RuntimeError('restoration mismatch')
                if not compile_only and not passed(run(name + '-restored', path, test, env), test):
                    item.update(status='INVALID', reason=item['reason'] + '; restored bound test failed')
        except Exception as error:
            item.update(status='INVALID', compiled=False, reason=f'{type(error).__name__}: {error}')
        results.append(item)
        print(f'{name}: {item["status"]}: {item["reason"]}', flush=True)
    counts = {state: sum(item['status'] == state for item in results)
              for state in ['KILLED', 'SURVIVED', 'INVALID', 'COMPILED']}
    (out / 'summary.json').write_text(json.dumps(dict(controls=results, counts=counts), indent=2) + '\n')
    print(json.dumps(counts), flush=True)
    return int(any(item['status'] != ('COMPILED' if compile_only else 'KILLED') for item in results))


if __name__ == '__main__':
    raise SystemExit(main())
