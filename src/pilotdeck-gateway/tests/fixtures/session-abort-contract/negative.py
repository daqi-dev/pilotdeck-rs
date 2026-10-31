#!/usr/bin/env python3
"""HEAD-guarded serial controls: [--compile-only] NEW_OUTPUT [mutation ...]."""
import json
import os
from pathlib import Path
import subprocess
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_process import mutating_sources, run_owned, wait_for_slot, new_output_directory
from negative_panic import named_test_killed
from negative_suite import Case, classify, invalidate_completed, passed, publish_summary


def main():
    rust = Path(__file__).resolve().parents[4]
    args = sys.argv[1:]
    compile_only = '--compile-only' in args
    args = [arg for arg in args if arg != '--compile-only']
    if not args or not os.environ.get('CARGO_TARGET_DIR'):
        raise SystemExit('set CARGO_TARGET_DIR; usage: [--compile-only] NEW_OUTPUT [mutation ...]')
    output = new_output_directory(args[0])
    assert os.environ.get('PDRS_BASELINE'), 'set PDRS_BASELINE'
    handle = 'pilotdeck-gateway/src/runtime/session/handle.rs'
    fixture = 'pilotdeck-gateway/tests/fixtures/session-abort-contract/oracle.json'
    test = 'runtime::session::handle::abort_contract_tests::abort_at_lease_and_dispose_windows_matches_ts'
    trace = 'lease/dispose abort trace; active=false'
    mutants = [
        ('suppress-abort-while-draining', handle,
         'if progress.state == HandleState::Disposed {',
         'if progress.state != HandleState::Active {', trace),
        ('disposed-before-lease-release', handle,
         'if let Some(mut lease) = guard.lease.take()',
         'progress.lock().unwrap_or_else(std::sync::PoisonError::into_inner).state = HandleState::Disposed;\n'
         '            if let Some(mut lease) = guard.lease.take()', trace),
        ('forward-abort-after-disposed', handle,
         'if progress.state == HandleState::Disposed {', 'if false {', trace),
        # Production order (TS createAgentSession.ts:229,234) releases the
        # project lease before disposing session resources. Reverting to the
        # pre-alignment Rust order opens the session window first, so the
        # lease window never opens; the replay is caught by its own designed
        # in-test 3-second bound, not by any external timeout.
        ('reverted-cleanup-order', handle,
         'let mut disposal_failures = Vec::new();\n'
         '                if let Some(mut lease) = guard.lease.take()\n'
         '                    && let Err(error) = lease.release().await\n'
         '                {\n'
         '                    disposal_failures.push(error);\n'
         '                }\n'
         '                if let Err(error) = session.dispose().await {\n'
         '                    disposal_failures.push(error);\n'
         '                }',
         'let mut disposal_failures = Vec::new();\n'
         '                if let Err(error) = session.dispose().await {\n'
         '                    disposal_failures.push(error);\n'
         '                }\n'
         '                if let Some(mut lease) = guard.lease.take()\n'
         '                    && let Err(error) = lease.release().await\n'
         '                {\n'
         '                    disposal_failures.push(error);\n'
         '                }', 'controlled dispose/lease interleaving hung'),
    ]
    originals = {name: (rust / name).read_text() for name in {m[1] for m in mutants} | {fixture}}
    oracle = json.loads(originals[fixture])
    source = 'src/agent/scope/AgentHandle.ts'
    mutants.append(('tampered-source-hash', fixture,
                    oracle['sourceSha256'][source], '0' * 64,
                    f'oracle hash for {source} does not match'))
    names = args[1:]
    if len(set(names)) != len(names) or set(names) - {m[0] for m in mutants}:
        raise SystemExit('unknown or duplicate mutation')
    selected = [m for m in mutants if not names or m[0] in names]
    env = dict(os.environ, CARGO_NET_OFFLINE='true', CARGO_TERM_QUIET='false')
    command = ['cargo', 'test', '--offline', '-p', 'pilotdeck-gateway', '--lib']

    def run(label, compile=False):
        gate = wait_for_slot(rust.parent / 'scripts/cargo-slots.sh', timeout=900)
        (output / f'{label}.slots.log').write_text(gate.stdout + gate.stderr)
        # #818: captured output keeps the named libtest status line intact
        # for both passed() and the shared named-panic kill criterion.
        command_args = ['--no-run'] if compile else [test, '--', '--exact']
        try:
            result = run_owned(command + command_args, cwd=rust, env=env, timeout=900)
        except subprocess.TimeoutExpired as error:
            (output / f'{label}.log').write_text(
                (error.stdout or '') + (error.stderr or '') + '\nTIMEOUT\n')
            raise
        (output / f'{label}.log').write_text(result.stdout + result.stderr)
        return result

    results = []
    try:
        with mutating_sources(rust.parent, [rust / m[1] for m in selected]) as snapshots:
            for name, file, before, after, marker in selected:
                path = rust / file
                row = dict(mutation=name, test=test, assertion=marker, compiled=False,
                           status='INVALID', reason='')
                try:
                    original = snapshots[path].decode()
                    if original.count(before) != 1 or before == after:
                        raise ValueError(f'{name}: expected one changing anchor')
                    if not compile_only and not passed(run(name + '-positive'), test):
                        raise RuntimeError('bound positive test failed')
                    path.write_text(original.replace(before, after, 1))
                    if run(name + '-compile', compile=True).returncode:
                        raise RuntimeError('mutant compilation failed')
                    row['compiled'] = True
                    if compile_only:
                        row.update(status='COMPILED', reason='mutant no-run passed')
                    else:
                        result = run(name + '-negative')
                        status, reason = classify(result, Case(name, path, before, after, test, marker))
                        if status == 'KILLED' and not named_test_killed(
                                result.stdout + result.stderr, test, marker):
                            status, reason = 'INVALID', 'missing bound assertion in named panic'
                        row.update(status=status, reason=reason)
                except Exception as error:
                    row.update(status='INVALID', reason=f'{type(error).__name__}: {error}')
                finally:
                    path.write_bytes(snapshots[path])
                if not compile_only:
                    try:
                        if not passed(run(name + '-restored'), test):
                            raise RuntimeError('restored bound test failed')
                    except Exception as error:
                        row.update(status='INVALID', reason=row['reason'] + f'; restored: {error}')
                results.append(row)
                print(f'{name}: {row["status"]}: {row["reason"]}', flush=True)
    except BaseException as error:
        invalidate_completed(results, f'source guard: {error}')
        recorded = {row['mutation'] for row in results}
        for name, file, before, after, marker in selected:
            if name not in recorded:
                results.append(dict(mutation=name, test=test, assertion=marker, compiled=False,
                                    status='INVALID', reason=f'source guard: {error}'))
        publish_summary(output, results)
        raise
    publish_summary(output, results)
    return int(any(row['status'] != ('COMPILED' if compile_only else 'KILLED') for row in results))


if __name__ == '__main__':
    raise SystemExit(main())
