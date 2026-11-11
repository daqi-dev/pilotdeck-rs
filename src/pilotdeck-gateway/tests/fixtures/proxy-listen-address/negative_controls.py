#!/usr/bin/env python3
"""#1131 HEAD-only process controls; every mutant must compile before judging."""
import argparse
import os
from pathlib import Path
import subprocess
import sys

ROOT = Path(__file__).resolve().parents[5]
FIXTURE = Path(__file__).resolve().parent
sys.path.insert(0, str(FIXTURE.parent))
from negative_process import mutating_sources, run_owned, wait_for_slot
from negative_suite import Case, classify, declares_test, passed, invalidate_completed, publish_summary
from negative_source import assertion_marker_present

SOURCE = ROOT / 'src/pilotdeck-gateway/src/main.rs'
TESTS = ROOT / 'src/pilotdeck-gateway/tests/proxy_listen_address.rs'
ORACLE = FIXTURE / 'oracle.json'
GATE = ROOT / 'src/pilotdeck-gateway/tests/common/baseline.rs'
TEST = 'reports_reachable_actual_address'
mutations = [
    ('requested-address', SOURCE,
     'let address = listener.local_addr()?;', 'let address = listen;',
     TEST, '#1131 reported port must be nonzero'),
    ('http-format', SOURCE,
     'println!("PilotDeck server listening: http://{address}");',
     'println!("proxy listening http://{address}");',
     TEST, '#1131 HTTP address prefix matches TS'),
    ('ws-format', SOURCE,
     'println!("WebSocket: ws://{address}/ws");',
     'println!("WebSocket: ws://{address}/wrong");',
     TEST, '#1131 stdout address bytes match TS'),
    ('ws-crlf', SOURCE,
     'println!("WebSocket: ws://{address}/ws");',
     'println!("WebSocket: ws://{address}/ws\\r");',
     TEST, '#1131 stdout address bytes match TS'),
    ('fixed-address', SOURCE,
     'let address = listener.local_addr()?;',
     'let address = if listen.port() == 0 { listener.local_addr()? } else { SocketAddr::new(listen.ip(), 1) };',
     TEST, '#1131 fixed address must be preserved'),
    ('unreachable-address', SOURCE,
     'let address = listener.local_addr()?;',
     'let address = listener.local_addr()?;\n    let replacement = TcpListener::bind("127.0.0.1:0").await?;\n    drop(listener);\n    let listener = replacement;',
     TEST, '#1131 reported address must accept TCP connections'),
    # The provenance gate is the only guard against a hand-edited or wrong-source
    # oracle; a drifted sourceCommit must fail the default (non-ignore) case.
    ('oracle-commit', ORACLE,
     '"sourceCommit": "cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85"',
     '"sourceCommit": "0000000000000000000000000000000000000000"',
     TEST, 'oracle baseline commit'),
]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('output', nargs='?', type=Path)
    parser.add_argument('--check', action='store_true')
    args = parser.parse_args()
    tests = TESTS.read_text()
    gate = GATE.read_text()
    # #2303 (条 4): every row is audited before the refusal, and a scanner
    # refusal (ValueError) becomes one named INVALID record per row instead
    # of a raw traceback on the first failure -- the session-factory /
    # session-drop-release audit shape.
    invalid = []
    for name, path, old, new, test, assertion in mutations:
        # The oracle-provenance mutant dies inside the shared baseline gate, so
        # its marker is bound against the gate's own source (the background-panic
        # provenance precedent); every other marker stays bound to the test file.
        markers = gate if path == ORACLE else tests
        try:
            if path.read_text().count(old) != 1 or old == new:
                raise ValueError('stale or invalid anchor')
            # #1890: one shared declaration rule instead of another bare-name
            # spelling. This crate's tests carry no module path (a single
            # integration-test file), so the binding is the file plus the name.
            # #2303 (条 5): the marker must live in code, never in a comment
            # -- the bare raw-text scan let a commented-out copy of the
            # assertion keep the binding green after the real one was gone.
            # The needle stays unquoted: the oracle-commit row's marker lives
            # inside the gate's formatted literal.
            if (not declares_test(tests, test)
                    or not assertion_marker_present(markers, assertion)):
                raise ValueError('stale test/assertion binding')
        except Exception as error:
            invalid.append(f'{name}: {type(error).__name__}: {error}')
    if invalid:
        raise RuntimeError('#1131 binding audit INVALID rows: ' + '; '.join(invalid))
    if args.check:
        print(f'#1131 {len(mutations)} anchors and test bindings valid')
        return 0
    if not args.output or not os.environ.get('CARGO_TARGET_DIR'):
        raise RuntimeError('#1131 output and caller CARGO_TARGET_DIR are required')
    out = args.output.resolve()
    out.mkdir(parents=True, exist_ok=False)
    env = dict(os.environ, CARGO_PROFILE_DEV_DEBUG='line-tables-only',
               CARGO_PROFILE_TEST_DEBUG='line-tables-only')
    env.pop('CARGO_TERM_QUIET', None)
    command = ['cargo', 'test', '--offline', '--locked', '--manifest-path', 'src/Cargo.toml',
               '-p', 'pilotdeck-gateway', '--test', 'proxy_listen_address']

    def run(label, *, compile_only=False):
        slot = wait_for_slot(ROOT / 'scripts/cargo-slots.sh', timeout=900)
        (out / f'{label}.slots.log').write_text(slot.stdout + slot.stderr)
        tail = ['--no-run'] if compile_only else [TEST, '--', '--exact']
        try:
            result = run_owned(command + tail, cwd=ROOT, env=env, timeout=900)
        except subprocess.TimeoutExpired as error:
            # A timed-out run still owns partial output; keep it as {label}.log
            # evidence instead of letting the exception discard it (#1131 fix-r1).
            raw = ''.join(value.decode(errors='replace') if isinstance(value, bytes) else value or ''
                          for value in (error.stdout, error.stderr))
            with (out / f'{label}.log').open('a') as log:
                log.write(raw + f'\n{type(error).__name__}: {error}\n')
            raise
        (out / f'{label}.log').write_text(result.stdout + result.stderr)
        return result

    results = []
    # The shared executors this script imports must also be pristine, or a
    # killed sibling runner's residue on them would execute as mutated bytes.
    # The provenance gate likewise: the default case runs baseline::oracle()
    # from it, so a locally weakened gate must fail here before any control
    # (fix-r2 P1: 7/7 KILLED must prove HEAD's full provenance criteria).
    guarded = [SOURCE, TESTS, Path(__file__).resolve(), ORACLE, GATE,
               FIXTURE.parent / 'negative_process.py',
               FIXTURE.parent / 'negative_suite.py']
    try:
        with mutating_sources(ROOT, guarded) as originals:
            for definition in mutations:
                case = Case(*definition)
                row = dict(mutation=case.mutation, test=case.test, assertion=case.assertion,
                           status='INVALID', reason='not run', compiled=False)
                try:
                    original = originals[case.path].decode()
                    if original.count(case.old) != 1:
                        raise RuntimeError('#1131 stale source anchor inside guard')
                    if not passed(run(case.mutation + '-positive'), case.test):
                        raise RuntimeError('#1131 positive control failed')
                    case.path.write_text(original.replace(case.old, case.new, 1))
                    if run(case.mutation + '-compile', compile_only=True).returncode != 0:
                        raise RuntimeError('#1131 mutant compilation failed; never a kill')
                    row['compiled'] = True
                    status, reason = classify(run(case.mutation + '-negative'), case)
                    row.update(status=status, reason=reason)
                except Exception as error:
                    row.update(status='INVALID', reason=f'{type(error).__name__}: {error}')
                finally:
                    case.path.write_bytes(originals[case.path])
                try:
                    if not passed(run(case.mutation + '-restored'), case.test):
                        raise RuntimeError('#1131 restored positive control failed')
                except Exception as error:
                    row.update(status='INVALID', reason=row['reason'] + f'; restored: {error}')
                results.append(row)
                print(f"{case.mutation}: {row['status']}: {row['reason']}", flush=True)
    except BaseException as error:
        reason = f'#1131 source guard: {type(error).__name__}: {error}'
        invalidate_completed(results, reason)
        for definition in mutations[len(results):]:
            results.append(dict(mutation=definition[0], test=definition[4], assertion=definition[5],
                                status='INVALID', reason=reason, compiled=False))
        publish_summary(out, results)
        print(reason, file=sys.stderr)
        return 1
    publish_summary(out, results)
    print('summary: ' + ', '.join(f'{status}={sum(row["status"] == status for row in results)}'
                                for status in ('KILLED', 'SURVIVED', 'INVALID')))
    return int(not results or any(row['status'] != 'KILLED' for row in results))


if __name__ == '__main__':
    sys.exit(main())
