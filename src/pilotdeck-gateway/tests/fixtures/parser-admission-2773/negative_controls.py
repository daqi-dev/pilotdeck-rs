#!/usr/bin/env python3
"""#2773 negative controls: the parser-admission call sites of both lanes.

Two no-op rows revert the #2152 wiring — the extracted sms call
(`channel/sms/admission.rs`) and the user_dialog store's serve-closure call
(`user_dialog/http_store.rs`) are each replaced by a disconnected comment, so
`FROB` / the hostless request reach the handler and the bound pin must fail
at its own assertion. The third row flips the gate's `Err` (CONNECT's
zero-byte close) into a routed answer, killed by the #2773 CONNECT pin; the
fourth turns the fail-closed `setHost` read into a fail-open one, killed by
the catch_unwind pin. Rows five and six (fix-r1) disconnect only the drain
wirings the P1 fixes added — the admission early-exit's (#2773 P1-b) and the
guard-431 mid-head trip's — so the bound lib pins' own FIN expectations fail
while the routed bytes still answer. One script holds both lanes because the
six rows are one mutation family of a single issue (#2773, review r2 +
fix-r1): the same node_parser_admission gate. The fix-r1 drain wirings
(rows five and six) exist only on the user_dialog side; the sms lane has no
drain wiring and no close-kind pin — its twins pin the bare 400 shape and
the CONNECT zero-byte destroy.

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
    # routed 405 against its recorded 400. The anchor spans the fix-r1 drain
    # wiring (#2773 P1-b) so the mutant removes it together with the call; the
    # bound assertion is cell-specific (`unknown-method:` prefix in the panic
    # payload), so a regression in another replay cell cannot satisfy this
    # row's anchor (review r2 boundary#3).
    ('parser-admission-noop', HTTP_STORE,
     '                                if let Some(rejection) =\n'
     '                                    parser_admission(&request, &timer)?\n'
     '                                {\n'
     '                                    // #2773 fix-r1 (P1-b): this early return\n'
     '                                    // is the one "close with an unread body"\n'
     '                                    // path that skipped the drain wiring the\n'
     '                                    // served responses get below — the unread\n'
     '                                    // bytes in the kernel queue turned the\n'
     '                                    // close into an RST instead of the FIN\n'
     '                                    // the serve path delivers (#702 P2).\n'
     '                                    // Mirror that wiring so the connection\n'
     '                                    // task drains before closing.\n'
     '                                    if body_pending\n'
     '                                        && rejection.headers().get(header::CONNECTION)\n'
     '                                            .is_some_and(|value| {\n'
     '                                                value.as_bytes()\n'
     '                                                    .eq_ignore_ascii_case(b"close")\n'
     '                                            })\n'
     '                                    {\n'
     '                                        timer.mark_unread_body_close();\n'
     '                                    }\n'
     '                                    return Ok(rejection);\n'
     '                                }\n',
     '                                // mutant: parser admission disconnected\n',
     'socket_requests_match_typescript_framing',
     'unknown-method: #2773 admission replay status differs'),
    # The admission gate's drain wiring alone, disconnected (#2773 fix-r1
    # P1-b): the early 400 still goes out, but the connection task no longer
    # drains the unread body, so the beats still in flight earn an RST. The
    # kill rides the pin's beat-write assertion first — the platform-
    # independent axis (#2773 fix-r3, review MM r3 concurrency#1/boundary#3);
    # on macOS the close-kind FIN assertion would also fire. This row is what
    # makes the ledger's "double kill" claim machine-checked (review r2
    # simplify#7).
    ('parser-admission-drain-unwired', HTTP_STORE,
     '                                    if body_pending\n'
     '                                        && rejection.headers().get(header::CONNECTION)\n'
     '                                            .is_some_and(|value| {\n'
     '                                                value.as_bytes()\n'
     '                                                    .eq_ignore_ascii_case(b"close")\n'
     '                                            })\n'
     '                                    {\n'
     '                                        timer.mark_unread_body_close();\n'
     '                                    }\n',
     '                                    if body_pending {\n'
     '                                        // mutant: drain wiring disconnected\n'
     '                                    }\n',
     'user_dialog::http_store::http_store_tests::'
     'parser_rejection_with_unread_body_delivers_and_closes',
     '#2774 the admission 400 must drain the unread body: '
     'every dribble beat is written, none reset'),
    # The guard-431 mid-head drain mark alone, disconnected (#2773 fix-r1):
    # the 431 still answers, but the unread head bytes turn the close into an
    # RST racing the response — the boundary pin's beat-write assertion fires
    # first (platform-independent axis, fix-r3 review MM r3), and on macOS
    # the FIN expectation would also fail.
    ('guard-431-drain-unwired', HTTP_STORE,
     '                        this.timer.mark_unread_body_close();\n'
     '                        response = Some(NODE_431_RESPONSE);\n',
     '                        response = Some(NODE_431_RESPONSE); '
     '// mutant: guard drain disconnected\n',
     'user_dialog::http_store::http_store_tests::'
     'header_guard_beats_hyper_buffer_cap_at_the_boundary',
     '#2773 the 431 must drain the unread head before closing: '
     'every dribble beat is written, none reset'),
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
