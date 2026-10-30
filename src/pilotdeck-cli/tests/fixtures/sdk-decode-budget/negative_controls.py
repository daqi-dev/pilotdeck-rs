#!/usr/bin/env python3
"""#983 HEAD-only decode-budget controls: --check | [--compile-only] NEW_OUTPUT_DIR [name ...].

Commit the patch before running mutants. CARGO_TARGET_DIR must be supplied by
caller; each mutant compiles, runs a nonce-bound test, restores and rechecks it.
The scripts run against committed sources only (mutating_sources refuses
uncommitted product files), so the coordinator runs them on the host after
landing the patch.
"""
from collections import namedtuple
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
from negative_suite import Case, ReceiptRun, classify, judge, passed

transport = root / 'src/pilotdeck-cli/src/sdk/transport.rs'
main_rs = root / 'src/pilotdeck-cli/src/main.rs'
codec = root / 'src/pilotdeck-protocol/src/codec.rs'
protocol_lib = root / 'src/pilotdeck-protocol/src/lib.rs'
sessions_list = root / 'src/pilotdeck-cli/tests/sdk_sessions_list_exit.rs'
sessions_run = root / 'src/pilotdeck-cli/tests/sdk_run_exit.rs'
# The mutated source and the cargo test target are separate bindings: the
# mutated product file's stem is not a test target (#983 fix-r1). `receipts`
# selects the judge: CLI rows assert through negative_receipt::verify, so a
# kill needs that run's nonce receipt; protocol rows assert with plain libtest
# messages, judged from the named test's own panic payload.
Mutation = namedtuple('Mutation', 'name path package target old new test assertion receipts')
mutations = [
    # take_field's missing-key fallback must stay Undefined, not Null: the
    # undefined-result boundary cell pins the CLI's error message.
    Mutation('take-result-undefined-fallback', transport, 'pilotdeck-cli',
             'sdk_sessions_list_exit',
             'fn take_field(mut frame: JsValue, name: &str) -> JsValue {\n    match &mut frame {\n        JsValue::Object(object) => object\n            .insert(name.into(), JsValue::Null)\n            .unwrap_or(JsValue::Undefined),',
             'fn take_field(mut frame: JsValue, name: &str) -> JsValue {\n    match &mut frame {\n        JsValue::Object(object) => object\n            .insert(name.into(), JsValue::Null)\n            .unwrap_or(JsValue::Null),',
             'sessions_list_transport_boundaries', '#983 transport boundary output undefined-result',
             True),
    # The streamed reindentation must keep json_pretty's ": " separator.
    Mutation('pretty-colon-space', main_rs, 'pilotdeck-cli', 'sdk_sessions_list_exit',
             "':' => out.write_all(b\": \")?,",
             "':' => out.write_all(b\":\")?,",
             'sessions_list_transport_boundaries', '#983 transport boundary output nested-projection',
             True),
    # ... and its two-space indentation.
    Mutation('pretty-indent', main_rs, 'pilotdeck-cli', 'sdk_sessions_list_exit',
             'out.write_all(b"  ")?;',
             'out.write_all(b" ")?;',
             'sessions_list_transport_boundaries', '#983 transport boundary output nested-projection',
             True),
    # stream_event must still extract the event payload through take_field.
    Mutation('stream-event-field', transport, 'pilotdeck-cli', 'sdk_run_exit',
             'return Ok(Some((take_field(frame, "event"), final_frame)));',
             'return Ok(Some((take_field(frame, "final"), final_frame)));',
             'run_matches_ts_matrix', '#1058 output bytes and exit', True),
    # The array continuation frame must close with ']' for an empty element:
    # the empty-container byte goldens pin the separator-free bytes.
    Mutation('stringify-array-close', codec, 'pilotdeck-protocol', 'codec',
             "output.push(']');",
             "output.push('}');",
             'empty_containers_stringify_exact_bytes', '#983 empty container bytes', False),
    # The boxed key index is what keeps a wide projection at 56 bytes per
    # element; inlining the map again grows every element to 96.
    Mutation('object-index-inline', protocol_lib, 'pilotdeck-protocol', 'codec',
             'indices: Box<HashMap<JsString, usize>>,',
             'indices: HashMap<JsString, usize>,',
             'wide_projections_stay_at_fifty_six_bytes_per_element',
             '#983 JsValue inline size', False),
]


def check(row):
    count = row.path.read_text().count(row.old)
    if count != 1 or row.old == row.new or not row.test or not row.assertion:
        raise ValueError(f'{row.name}: stale/invalid anchor: expected=1 actual={count}')
    target = root / 'src' / row.package / 'tests' / f'{row.target}.rs'
    if not target.is_file():
        raise ValueError(f'{row.name}: cargo test target missing: {target.relative_to(root)}')
    if row.test not in target.read_text():
        raise ValueError(
            f'{row.name}: bound test {row.test} not declared in {target.relative_to(root)}')


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

    def run(label, row, run_env, *, compile=False):
        slots = wait_for_slot(root / 'scripts/cargo-slots.sh', timeout=600, interval=15)
        (out / f'{label}.slots.log').write_text(slots.stdout + slots.stderr)
        command = ['cargo', 'test', '--offline', '--manifest-path', 'src/Cargo.toml',
                   '-p', row.package, '--test', row.target]
        command += (['--no-run'] if compile else
                    [row.test, '--', '--exact', '--show-output', '--test-threads=1'])
        result = run_owned(command, cwd=root, env=run_env, timeout=900)
        (out / f'{label}.stdout').write_text(result.stdout)
        (out / f'{label}.stderr').write_text(result.stderr)
        return result

    guarded = [transport, main_rs, codec, protocol_lib, sessions_list, sessions_run,
               Path(__file__).resolve(), root / 'src/pilotdeck-cli/Cargo.toml',
               root / 'src/pilotdeck-protocol/tests/codec.rs',
               root / 'src/pilotdeck-protocol/Cargo.toml', root / 'src/Cargo.toml',
               # r4: every other input the bound tests and the judge read (#983).
               root / 'src/pilotdeck-cli/tests/common/negative_receipt.rs',
               root / 'src/pilotdeck-cli/tests/fixtures/sdk-run/oracle.json',
               root / 'src/pilotdeck-cli/tests/fixtures/sdk-sessions-list/oracle.json',
               root / 'src/pilotdeck-cli/tests/fixtures/sdk-transport-limits/oracle.json',
               shared / 'negative_panic.py', shared / 'pending_reanchor.py',
               root / 'scripts/cargo-slots.sh',
               root / 'src/Cargo.lock', shared / 'negative_process.py',
               shared / 'negative_suite.py']
    results = []
    for row in selected:
        item = dict(name=row.name, test=row.test, status='INVALID', reason='not run',
                    compiled=False)
        try:
            check(row)
            with mutating_sources(root, guarded) as originals:
                original = originals[row.path].decode()
                try:
                    if not compile_only and not passed(run(row.name + '-positive', row, env),
                                                        row.test):
                        raise RuntimeError('positive bound test failed')
                    changed = original.replace(row.old, row.new, 1)
                    (out / f'{row.name}.patch').write_text(''.join(difflib.unified_diff(
                        original.splitlines(True), changed.splitlines(True),
                        fromfile=str(row.path.relative_to(root)),
                        tofile=str(row.path.relative_to(root)))))
                    row.path.write_text(changed)
                    if run(row.name + '-compile', row, env, compile=True).returncode != 0:
                        raise RuntimeError('mutant compile failed; not evidence')
                    item['compiled'] = True
                    if compile_only:
                        item.update(status='COMPILED', reason='mutant no-run passed')
                    elif row.receipts:
                        receipts = ReceiptRun(out, row.name + '-negative',
                                              [(row.test, row.assertion)])
                        result = run(row.name + '-negative', row, receipts.environment(env))
                        killed, reason, _ = judge(result, '', [(row.test, row.assertion)],
                                                  receipts)
                        item.update(status='KILLED' if killed else
                                    ('SURVIVED' if passed(result, row.test) else 'INVALID'),
                                    reason=reason)
                    else:
                        result = run(row.name + '-negative', row, env)
                        status, reason = classify(result, Case(
                            row.name, row.path, row.old, row.new, row.test, row.assertion))
                        item.update(status=status, reason=reason)
                finally:
                    # Only restore bytes supplied by the official HEAD guard.
                    row.path.write_bytes(originals[row.path])
                    if row.path.read_bytes() != originals[row.path]:
                        raise RuntimeError('restoration mismatch')
                if not compile_only and not passed(run(row.name + '-restored', row, env),
                                                    row.test):
                    item.update(status='INVALID', reason=item['reason'] +
                                '; restored bound test failed')
        except Exception as error:
            item.update(status='INVALID', compiled=False, reason=f'{type(error).__name__}: {error}')
        results.append(item)
        print(f'{row.name}: {item["status"]}: {item["reason"]}', flush=True)
    counts = {state: sum(item['status'] == state for item in results)
              for state in ['KILLED', 'SURVIVED', 'INVALID', 'COMPILED']}
    (out / 'summary.json').write_text(json.dumps(dict(controls=results, counts=counts),
                                                 indent=2) + '\n')
    print(json.dumps(counts), flush=True)
    return int(any(item['status'] != ('COMPILED' if compile_only else 'KILLED')
                   for item in results))


if __name__ == '__main__':
    raise SystemExit(main())
