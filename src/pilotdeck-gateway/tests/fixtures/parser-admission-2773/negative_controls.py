#!/usr/bin/env python3
"""#2773 negative controls: the parser-admission call sites of both lanes.

Two no-op rows revert the #2152 wiring — the extracted sms call
(`channel/sms/admission.rs`) and the user_dialog store's serve-closure call
(`user_dialog/http_store.rs`) are each replaced by a disconnected comment, so
`FROB` / the hostless request reach the handler and the bound pin must fail
at its own assertion. The third row flips the gate's `Err` (CONNECT's
zero-byte close) into a routed answer, killed by the #2773 CONNECT pin; the
fourth turns the fail-closed `setHost` read into a fail-open one, killed by
the catch_unwind pin. One script holds both lanes because the four rows are
one mutation family of a single issue (#2773, review r2): the same
node_parser_admission gate.

Run after committing the candidate (the mutating_sources guard refuses a
dirty tree). Each mutant is applied to the original one at a time, never
cumulatively, and every mutation is restored and byte-verified afterwards;
an expired anchor (count != 1) aborts the row as INVALID instead of
SURVIVED.

    CARGO_TARGET_DIR=... python3 negative_controls.py NEW_OUTPUT_DIR [name ...]
"""
import argparse
import json
import os
from pathlib import Path
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_process import (CARGO_SLOTS_WAIT_SECONDS, mutating_sources,
                               run_owned, wait_for_slot)
from negative_suite import Case, classify, passed

root = Path(__file__).resolve().parents[5]
SMS_ADMISSION = root / 'src/pilotdeck-gateway/src/channel/sms/admission.rs'
HTTP_STORE = root / 'src/pilotdeck-gateway/src/user_dialog/http_store.rs'
INTEGRATION = root / 'src/pilotdeck-gateway/tests/user_dialog_http_store.rs'

# (name, source, before, after, bound test, bound assertion). Every bound
# assertion is a message owned by the named test, so only that test's own
# panic payload can produce a KILLED verdict. A `::` in the test name means a
# lib test (`--lib`); a bare name is an integration test of the
# user_dialog_http_store binary.
cases = [
    # The sms call, no-op'd: FROB reaches the route guard and answers a
    # routed status, not the bare parser 400.
    ('sms-admission-noop', SMS_ADMISSION,
     '    crate::native_http::node_parser_admission(request)'
     '.map_err(|_| super::DestroyConnection)\n',
     '    Ok(None) // mutant: parser admission disconnected\n',
     'channel::sms::tests::unknown_method_is_the_bare_parser_400',
     "#2152 pin unknown-method: llhttp's bare 400, not a routed answer"),
    # The `Err` branch answers instead of destroying: a CONNECT then draws
    # routed bytes instead of the zero-byte close.
    ('sms-admission-err-responds', SMS_ADMISSION,
     '    crate::native_http::node_parser_admission(request)'
     '.map_err(|_| super::DestroyConnection)\n',
     '    Ok(crate::native_http::node_parser_admission(request)'
     '.ok().flatten()) // mutant: CONNECT answered\n',
     'channel::sms::tests::connect_pin_is_the_zero_byte_destroy',
     '#2773 pin connect'),
    # The user_dialog call, no-op'd: the unknown-method replay cell draws a
    # routed 405 against its recorded 400.
    ('parser-admission-noop', HTTP_STORE,
     '                                if let Some(rejection) =\n'
     '                                    parser_admission(&request, &timer)?\n'
     '                                {\n'
     '                                    return Ok(rejection);\n'
     '                                }\n',
     '                                // mutant: parser admission disconnected\n',
     'socket_requests_match_typescript_framing',
     '#2773 admission replay status differs'),
    # The setHost read, fail-open: the non-boolean is no longer refused, so
    # the catch_unwind pin finds no panic and fails at its own expectation.
    ('set-host-fail-open', INTEGRATION,
     '        Some(other) => {\n'
     '            panic!("#2152 request `setHost` must be a boolean, '
     'found {other:?} ({name})")\n'
     '        }\n',
     '        Some(other) => {\n'
     '            let _ = (other, name);\n'
     '            true\n'
     '        }\n',
     'socket_request_set_host_refuses_a_non_boolean',
     '#2773 the non-boolean `setHost` must be refused fail-closed'),
]


def test_args(full):
    if '::' in full:
        return ['--lib', full]
    return ['--test', 'user_dialog_http_store', full]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
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
    command = ['cargo', 'test', '--manifest-path', 'src/Cargo.toml',
               '-p', 'pilotdeck-gateway']
    results = []

    def run(label, full):
        # #1908: the wait budget must exceed the single-sample cap
        slot = wait_for_slot(root / 'scripts/cargo-slots.sh', timeout=CARGO_SLOTS_WAIT_SECONDS)
        (args.out / f'{label}.slots.log').write_text(slot.stdout + slot.stderr)
        result = run_owned(command + test_args(full) + ['--', '--exact'],
                           cwd=root, env=env, timeout=900)
        (args.out / f'{label}.log').write_text(result.stdout + result.stderr)
        return result

    def record(name, full, assertion, status, reason):
        results.append(dict(mutation=name, test=full, assertion=assertion,
                            status=status, reason=reason))
        print(f'{name}: {status}: {reason}', flush=True)

    sources = sorted({row[1] for row in selected})
    try:
        with mutating_sources(root, sources) as snapshots:
            for name, path, old, new, test, assertion in selected:
                full = test
                status, reason = 'INVALID', 'not run'
                try:
                    original = snapshots[path].decode()
                    count = original.count(old)
                    if count != 1:
                        raise ValueError(
                            f'{name}: expected one source anchor, found {count}')
                    if not passed(run(name + '-positive', full), full):
                        reason = 'bound positive test did not pass'
                    else:
                        path.write_text(original.replace(old, new, 1))
                        status, reason = classify(run(name + '-negative', full),
                                                  Case(name, path, old, new,
                                                       full, assertion))
                except Exception as error:
                    status, reason = 'INVALID', f'{type(error).__name__}: {error}'
                finally:
                    path.write_bytes(snapshots[path])
                try:
                    if not passed(run(name + '-restored', full), full):
                        status, reason = 'INVALID', reason + '; restored bound test did not pass'
                except Exception as error:
                    status, reason = 'INVALID', reason + f'; restored: {error}'
                record(name, full, assertion, status, reason)
    except Exception as error:
        reason = f'source guard: {error}'
        for row in results:
            row.update(status='INVALID', reason=row['reason'] + '; ' + reason)
        for name, _path, _old, _new, test, assertion in selected[len(results):]:
            record(name, test, assertion, 'INVALID', reason)
    counts = {state: sum(row['status'] == state for row in results)
              for state in ('KILLED', 'SURVIVED', 'INVALID')}
    (args.out / 'summary.json').write_text(
        json.dumps(dict(controls=results, counts=counts), indent=2) + '\n')
    print(json.dumps(counts), flush=True)
    return int(not results or any(row['status'] != 'KILLED' for row in results))


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
