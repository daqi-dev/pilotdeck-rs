#!/usr/bin/env python3
"""#1075 HEAD-only stdin controls: --check | --self-check |
[--compile-only] NEW_OUTPUT_DIR [name ...].

Commit the patch before running mutants. CARGO_TARGET_DIR must be supplied by
caller; each mutant compiles, runs a nonce-bound test, restores and rechecks it.
"""
import difflib
import io
import json
import os
from pathlib import Path
import sys

sys.dont_write_bytecode = True
root = Path(__file__).resolve().parents[5]
shared = root / 'src/pilotdeck-gateway/tests/fixtures'
sys.path.insert(0, str(shared))
from negative_process import mutating_sources, run_owned, wait_for_slot, write_source_atomic
from negative_suite import ReceiptRun, judge, passed

# #1044/#1179: every family's fixture bodies live in tests/common/sdk_exit.rs;
# rows carry an explicit cargo --test target because the shared module's stem is
# not a test binary. The exit-code mutant on the shared module cannot rely on
# the per-file ExitStatusExt import, so it calls the trait by path.
settings_target = 'sdk_settings_exit'
sessions_list_target = 'sdk_sessions_list_exit'
common = root / 'src/pilotdeck-cli/tests/common/sdk_exit.rs'
mutations = [
    ('settings-broken-pipe', common, settings_target, 'Err(error) if error.kind() == io::ErrorKind::BrokenPipe => {}', 'Err(error) if false && error.kind() == io::ErrorKind::BrokenPipe => {}', 'fixture_stdin_broken_pipe_preserves_output', '#1075 broken pipe must not panic'),
    ('settings-other-error', common, settings_target, 'Err(error) => panic!("#1075 fixture stdin: {error:?}"),', 'Err(_error) => {},', 'fixture_stdin_other_errors_still_fail', '#1075 other stdin errors must fail'),
    ('settings-skip-write', common, settings_target, 'match stdin.write_all(data) {', 'match { let _ = (stdin, data); Ok::<(), io::Error>(()) } {', 'fixture_stdin_writes_all_bytes', '#1075 complete stdin write'),
    ('sessions-list-broken-pipe', common, sessions_list_target, 'Err(error) if error.kind() == io::ErrorKind::BrokenPipe => {}', 'Err(error) if false && error.kind() == io::ErrorKind::BrokenPipe => {}', 'fixture_stdin_broken_pipe_preserves_output', '#1075 broken pipe must not panic'),
    ('sessions-list-other-error', common, sessions_list_target, 'Err(error) => panic!("#1075 fixture stdin: {error:?}"),', 'Err(_error) => {},', 'fixture_stdin_other_errors_still_fail', '#1075 other stdin errors must fail'),
    ('sessions-list-skip-write', common, sessions_list_target, 'match stdin.write_all(data) {', 'match { let _ = (stdin, data); Ok::<(), io::Error>(()) } {', 'fixture_stdin_writes_all_bytes', '#1075 complete stdin write'),
    ('sessions-messages-broken-pipe', common, 'sdk_sessions_messages_exit', 'Err(error) if error.kind() == io::ErrorKind::BrokenPipe => {}', 'Err(error) if false && error.kind() == io::ErrorKind::BrokenPipe => {}', 'fixture_stdin_broken_pipe_preserves_output', '#1075 broken pipe must not panic'),
    ('sessions-messages-other-error', common, 'sdk_sessions_messages_exit', 'Err(error) => panic!("#1075 fixture stdin: {error:?}"),', 'Err(_error) => {},', 'fixture_stdin_other_errors_still_fail', '#1075 other stdin errors must fail'),
    ('sessions-messages-skip-write', common, 'sdk_sessions_messages_exit', 'match stdin.write_all(data) {', 'match { let _ = (stdin, data); Ok::<(), io::Error>(()) } {', 'fixture_stdin_writes_all_bytes', '#1075 complete stdin write'),
    ('settings-exit-code', common, settings_target, 'collect_bounded(child, BOUND).await', '{ let mut output = collect_bounded(child, BOUND).await; output.status = std::os::unix::process::ExitStatusExt::from_raw(0); output }', 'fixture_stdin_broken_pipe_preserves_output', '#1075 preserve exit code'),
    ('settings-stdout', common, settings_target, 'collect_bounded(child, BOUND).await', '{ let mut output = collect_bounded(child, BOUND).await; output.stdout.clear(); output }', 'fixture_stdin_broken_pipe_preserves_output', '#1075 preserve stdout'),
    ('settings-stderr', common, settings_target, 'collect_bounded(child, BOUND).await', '{ let mut output = collect_bounded(child, BOUND).await; output.stderr.clear(); output }', 'fixture_stdin_broken_pipe_preserves_output', '#1075 preserve stderr'),
    ('sessions-list-exit-code', common, sessions_list_target, 'collect_bounded(child, BOUND).await', '{ let mut output = collect_bounded(child, BOUND).await; output.status = std::os::unix::process::ExitStatusExt::from_raw(0); output }', 'fixture_stdin_broken_pipe_preserves_output', '#1075 preserve exit code'),
    ('sessions-list-stdout', common, sessions_list_target, 'collect_bounded(child, BOUND).await', '{ let mut output = collect_bounded(child, BOUND).await; output.stdout.clear(); output }', 'fixture_stdin_broken_pipe_preserves_output', '#1075 preserve stdout'),
    ('sessions-list-stderr', common, sessions_list_target, 'collect_bounded(child, BOUND).await', '{ let mut output = collect_bounded(child, BOUND).await; output.stderr.clear(); output }', 'fixture_stdin_broken_pipe_preserves_output', '#1075 preserve stderr'),
    ('sessions-messages-exit-code', common, 'sdk_sessions_messages_exit', 'collect_bounded(child, BOUND).await', '{ let mut output = collect_bounded(child, BOUND).await; output.status = std::os::unix::process::ExitStatusExt::from_raw(0); output }', 'fixture_stdin_broken_pipe_preserves_output', '#1075 preserve exit code'),
    ('sessions-messages-stdout', common, 'sdk_sessions_messages_exit', 'collect_bounded(child, BOUND).await', '{ let mut output = collect_bounded(child, BOUND).await; output.stdout.clear(); output }', 'fixture_stdin_broken_pipe_preserves_output', '#1075 preserve stdout'),
    ('sessions-messages-stderr', common, 'sdk_sessions_messages_exit', 'collect_bounded(child, BOUND).await', '{ let mut output = collect_bounded(child, BOUND).await; output.stderr.clear(); output }', 'fixture_stdin_broken_pipe_preserves_output', '#1075 preserve stderr'),
    ('run-broken-pipe', common, 'sdk_run_exit', 'Err(error) if error.kind() == io::ErrorKind::BrokenPipe => {}', 'Err(error) if false && error.kind() == io::ErrorKind::BrokenPipe => {}', 'fixture_stdin_broken_pipe_preserves_output', '#1075 broken pipe must not panic'),
    ('run-other-error', common, 'sdk_run_exit', 'Err(error) => panic!("#1075 fixture stdin: {error:?}"),', 'Err(_error) => {},', 'fixture_stdin_other_errors_still_fail', '#1075 other stdin errors must fail'),
    ('run-skip-write', common, 'sdk_run_exit', 'match stdin.write_all(data) {', 'match { let _ = (stdin, data); Ok::<(), io::Error>(()) } {', 'fixture_stdin_writes_all_bytes', '#1075 complete stdin write'),
    ('run-exit-code', common, 'sdk_run_exit', 'collect_bounded(child, BOUND).await', '{ let mut output = collect_bounded(child, BOUND).await; output.status = std::os::unix::process::ExitStatusExt::from_raw(0); output }', 'fixture_stdin_broken_pipe_preserves_output', '#1075 preserve exit code'),
    ('run-stdout', common, 'sdk_run_exit', 'collect_bounded(child, BOUND).await', '{ let mut output = collect_bounded(child, BOUND).await; output.stdout.clear(); output }', 'fixture_stdin_broken_pipe_preserves_output', '#1075 preserve stdout'),
    ('run-stderr', common, 'sdk_run_exit', 'collect_bounded(child, BOUND).await', '{ let mut output = collect_bounded(child, BOUND).await; output.stderr.clear(); output }', 'fixture_stdin_broken_pipe_preserves_output', '#1075 preserve stderr'),
    ('format-broken-pipe', common, 'sdk_format_exit', 'Err(error) if error.kind() == io::ErrorKind::BrokenPipe => {}', 'Err(error) if false && error.kind() == io::ErrorKind::BrokenPipe => {}', 'fixture_stdin_broken_pipe_preserves_output', '#1075 broken pipe must not panic'),
    ('format-other-error', common, 'sdk_format_exit', 'Err(error) => panic!("#1075 fixture stdin: {error:?}"),', 'Err(_error) => {},', 'fixture_stdin_other_errors_still_fail', '#1075 other stdin errors must fail'),
    ('format-skip-write', common, 'sdk_format_exit', 'match stdin.write_all(data) {', 'match { let _ = (stdin, data); Ok::<(), io::Error>(()) } {', 'fixture_stdin_writes_all_bytes', '#1075 complete stdin write'),
    ('format-exit-code', common, 'sdk_format_exit', 'collect_bounded(child, BOUND).await', '{ let mut output = collect_bounded(child, BOUND).await; output.status = std::os::unix::process::ExitStatusExt::from_raw(0); output }', 'fixture_stdin_broken_pipe_preserves_output', '#1075 preserve exit code'),
    ('format-stdout', common, 'sdk_format_exit', 'collect_bounded(child, BOUND).await', '{ let mut output = collect_bounded(child, BOUND).await; output.stdout.clear(); output }', 'fixture_stdin_broken_pipe_preserves_output', '#1075 preserve stdout'),
    ('format-stderr', common, 'sdk_format_exit', 'collect_bounded(child, BOUND).await', '{ let mut output = collect_bounded(child, BOUND).await; output.stderr.clear(); output }', 'fixture_stdin_broken_pipe_preserves_output', '#1075 preserve stderr'),
    ('resume-broken-pipe', common, 'sdk_resume_exit', 'Err(error) if error.kind() == io::ErrorKind::BrokenPipe => {}', 'Err(error) if false && error.kind() == io::ErrorKind::BrokenPipe => {}', 'fixture_stdin_broken_pipe_preserves_output', '#1075 broken pipe must not panic'),
    ('resume-other-error', common, 'sdk_resume_exit', 'Err(error) => panic!("#1075 fixture stdin: {error:?}"),', 'Err(_error) => {},', 'fixture_stdin_other_errors_still_fail', '#1075 other stdin errors must fail'),
    ('resume-skip-write', common, 'sdk_resume_exit', 'match stdin.write_all(data) {', 'match { let _ = (stdin, data); Ok::<(), io::Error>(()) } {', 'fixture_stdin_writes_all_bytes', '#1075 complete stdin write'),
    ('resume-exit-code', common, 'sdk_resume_exit', 'collect_bounded(child, BOUND).await', '{ let mut output = collect_bounded(child, BOUND).await; output.status = std::os::unix::process::ExitStatusExt::from_raw(0); output }', 'fixture_stdin_broken_pipe_preserves_output', '#1075 preserve exit code'),
    ('resume-stdout', common, 'sdk_resume_exit', 'collect_bounded(child, BOUND).await', '{ let mut output = collect_bounded(child, BOUND).await; output.stdout.clear(); output }', 'fixture_stdin_broken_pipe_preserves_output', '#1075 preserve stdout'),
    ('resume-stderr', common, 'sdk_resume_exit', 'collect_bounded(child, BOUND).await', '{ let mut output = collect_bounded(child, BOUND).await; output.stderr.clear(); output }', 'fixture_stdin_broken_pipe_preserves_output', '#1075 preserve stderr'),
]


def check(row):
    name, path, target, old, new, test, assertion = row
    count = path.read_text().count(old)
    if count != 1 or old == new or not test or not assertion:
        raise ValueError(f'{name}: stale/invalid anchor: expected=1 actual={count}')


def check_anchors(stdout=None, stderr=None):
    """Return 0 only for a non-empty manifest whose every anchor is valid.

    #1364 P0-a: an empty manifest used to print ``0/0 valid`` and exit 0, so a
    deleted mutation table looked green -- zero anchors verified, gate passed.
    """
    stdout = sys.stdout if stdout is None else stdout
    stderr = sys.stderr if stderr is None else stderr
    errors = []
    for row in mutations:
        try:
            check(row)
        except Exception as error:
            errors.append(str(error))
    for error in errors:
        print('INVALID ' + error, file=stdout)
    if not mutations:
        print('#1075 anchors: empty mutation manifest; refusing to pass', file=stderr)
        return 2
    print(f'#1075 anchors: {len(mutations) - len(errors)}/{len(mutations)} valid', file=stdout)
    return int(bool(errors))


def self_check():
    """Negative control for this gate: an empty manifest must fail closed (#1364 P0-a)."""
    if not mutations:
        print('self-check FAILED: the shipped mutation manifest is empty', file=sys.stderr)
        return 1
    saved = list(mutations)
    out, err = io.StringIO(), io.StringIO()
    try:
        mutations.clear()
        status = check_anchors(stdout=out, stderr=err)
    finally:
        mutations[:] = saved
    if status == 0:
        print('self-check FAILED: an empty manifest passed the anchor gate', file=sys.stderr)
        return 1
    if 'empty mutation manifest' not in err.getvalue():
        print('self-check FAILED: empty manifest did not report the explicit error',
              file=sys.stderr)
        return 1
    print(f'self-check: empty manifest fails closed (rc={status}); '
          f'shipped manifest still has {len(mutations)} anchors: ok')
    return 0


def main():
    args = sys.argv[1:]
    if args == ['--check']:
        return check_anchors()
    if args == ['--self-check']:
        return self_check()
    if not mutations:
        raise SystemExit('#1075 empty mutation manifest: nothing to run; refusing to pass')
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

    def run(label, target, test, run_env, *, compile=False):
        slots = wait_for_slot(root / 'scripts/cargo-slots.sh', timeout=600, interval=15)
        (out / f'{label}.slots.log').write_text(slots.stdout + slots.stderr)
        command = ['cargo', 'test', '--offline', '--manifest-path', 'src/Cargo.toml',
                   '-p', 'pilotdeck-cli', '--test', target]
        command += (['--no-run'] if compile else
                    [test, '--', '--exact', '--show-output', '--test-threads=1'])
        result = run_owned(command, cwd=root, env=run_env, timeout=900)
        (out / f'{label}.stdout').write_text(result.stdout)
        (out / f'{label}.stderr').write_text(result.stderr)
        return result

    guarded = [common, Path(__file__).resolve(),
               root / 'src/pilotdeck-cli/Cargo.toml', root / 'src/Cargo.lock',
               shared / 'negative_process.py', shared / 'negative_suite.py']
    results = []
    for row in selected:
        name, path, target, old, new, test, assertion = row
        item = dict(name=name, test=test, status='INVALID', reason='not run', compiled=False)
        try:
            check(row)
            with mutating_sources(root, guarded) as originals:
                original = originals[path].decode()
                try:
                    if not compile_only and not passed(run(name + '-positive', target, test, env), test):
                        raise RuntimeError('positive bound test failed')
                    changed = original.replace(old, new, 1)
                    (out / f'{name}.patch').write_text(''.join(difflib.unified_diff(
                        original.splitlines(True), changed.splitlines(True),
                        fromfile=str(path.relative_to(root)), tofile=str(path.relative_to(root)))))
                    path.write_text(changed)
                    if run(name + '-compile', target, test, env, compile=True).returncode != 0:
                        raise RuntimeError('mutant compile failed; not evidence')
                    item['compiled'] = True
                    if compile_only:
                        item.update(status='COMPILED', reason='mutant no-run passed')
                    else:
                        receipts = ReceiptRun(out, name + '-negative', [(test, assertion)])
                        result = run(name + '-negative', target, test, receipts.environment(env))
                        killed, reason, _ = judge(result, '', [(test, assertion)], receipts)
                        item.update(status='KILLED' if killed else
                                    ('SURVIVED' if passed(result, test) else 'INVALID'), reason=reason)
                finally:
                    # Only restore bytes supplied by the official HEAD guard.
                    # #1903: one atomic rename, never a truncating write -- a
                    # SIGKILL mid-restore must not leave a half-written common
                    # module that fails every sibling suite's baseline.
                    write_source_atomic(root, path, originals[path])
                    if path.read_bytes() != originals[path]:
                        raise RuntimeError('restoration mismatch')
                if not compile_only and not passed(run(name + '-restored', target, test, env), test):
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
