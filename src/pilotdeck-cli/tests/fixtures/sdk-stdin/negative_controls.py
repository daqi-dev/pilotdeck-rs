#!/usr/bin/env python3
"""#1075 HEAD-only stdin controls: --check | [--compile-only] NEW_OUTPUT_DIR [name ...].

Commit the patch before running mutants. CARGO_TARGET_DIR must be supplied by
caller; each mutant compiles, runs a nonce-bound test, restores and rechecks it.
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

# #1044: settings/sessions-list fixture bodies moved to tests/common/sdk_exit.rs;
# rows carry an explicit cargo --test target because the shared module's stem is
# not a test binary. The exit-code mutant on the shared module cannot rely on
# the per-file ExitStatusExt import, so it calls the trait by path.
settings_target = 'sdk_settings_exit'
sessions_list_target = 'sdk_sessions_list_exit'
common = root / 'src/pilotdeck-cli/tests/common/sdk_exit.rs'
sessions_messages = root / 'src/pilotdeck-cli/tests/sdk_sessions_messages_exit.rs'
mutations = [
    ('settings-broken-pipe', common, settings_target, 'Err(error) if error.kind() == io::ErrorKind::BrokenPipe => {}', 'Err(error) if false && error.kind() == io::ErrorKind::BrokenPipe => {}', 'fixture_stdin_broken_pipe_preserves_output', '#1075 broken pipe must not panic'),
    ('settings-other-error', common, settings_target, 'Err(error) => panic!("#1075 fixture stdin: {error:?}"),', 'Err(_error) => {},', 'fixture_stdin_other_errors_still_fail', '#1075 other stdin errors must fail'),
    ('settings-skip-write', common, settings_target, 'match stdin.write_all(data) {', 'match { let _ = (stdin, data); Ok::<(), io::Error>(()) } {', 'fixture_stdin_writes_all_bytes', '#1075 complete stdin write'),
    ('sessions-list-broken-pipe', common, sessions_list_target, 'Err(error) if error.kind() == io::ErrorKind::BrokenPipe => {}', 'Err(error) if false && error.kind() == io::ErrorKind::BrokenPipe => {}', 'fixture_stdin_broken_pipe_preserves_output', '#1075 broken pipe must not panic'),
    ('sessions-list-other-error', common, sessions_list_target, 'Err(error) => panic!("#1075 fixture stdin: {error:?}"),', 'Err(_error) => {},', 'fixture_stdin_other_errors_still_fail', '#1075 other stdin errors must fail'),
    ('sessions-list-skip-write', common, sessions_list_target, 'match stdin.write_all(data) {', 'match { let _ = (stdin, data); Ok::<(), io::Error>(()) } {', 'fixture_stdin_writes_all_bytes', '#1075 complete stdin write'),
    ('sessions-messages-broken-pipe', sessions_messages, 'sdk_sessions_messages_exit', 'Err(error) if error.kind() == io::ErrorKind::BrokenPipe => {}', 'Err(error) if false && error.kind() == io::ErrorKind::BrokenPipe => {}', 'fixture_stdin_broken_pipe_preserves_output', '#1075 broken pipe must not panic'),
    ('sessions-messages-other-error', sessions_messages, 'sdk_sessions_messages_exit', 'Err(error) => panic!("#1075 fixture stdin: {error:?}"),', 'Err(_error) => {},', 'fixture_stdin_other_errors_still_fail', '#1075 other stdin errors must fail'),
    ('sessions-messages-skip-write', sessions_messages, 'sdk_sessions_messages_exit', 'match stdin.write_all(data) {', 'match { let _ = (stdin, data); Ok::<(), io::Error>(()) } {', 'fixture_stdin_writes_all_bytes', '#1075 complete stdin write'),
    ('settings-exit-code', common, settings_target, 'collect_bounded(child, BOUND).await', '{ let mut output = collect_bounded(child, BOUND).await; output.status = std::os::unix::process::ExitStatusExt::from_raw(0); output }', 'fixture_stdin_broken_pipe_preserves_output', '#1075 preserve exit code'),
    ('settings-stdout', common, settings_target, 'collect_bounded(child, BOUND).await', '{ let mut output = collect_bounded(child, BOUND).await; output.stdout.clear(); output }', 'fixture_stdin_broken_pipe_preserves_output', '#1075 preserve stdout'),
    ('settings-stderr', common, settings_target, 'collect_bounded(child, BOUND).await', '{ let mut output = collect_bounded(child, BOUND).await; output.stderr.clear(); output }', 'fixture_stdin_broken_pipe_preserves_output', '#1075 preserve stderr'),
    ('sessions-list-exit-code', common, sessions_list_target, 'collect_bounded(child, BOUND).await', '{ let mut output = collect_bounded(child, BOUND).await; output.status = std::os::unix::process::ExitStatusExt::from_raw(0); output }', 'fixture_stdin_broken_pipe_preserves_output', '#1075 preserve exit code'),
    ('sessions-list-stdout', common, sessions_list_target, 'collect_bounded(child, BOUND).await', '{ let mut output = collect_bounded(child, BOUND).await; output.stdout.clear(); output }', 'fixture_stdin_broken_pipe_preserves_output', '#1075 preserve stdout'),
    ('sessions-list-stderr', common, sessions_list_target, 'collect_bounded(child, BOUND).await', '{ let mut output = collect_bounded(child, BOUND).await; output.stderr.clear(); output }', 'fixture_stdin_broken_pipe_preserves_output', '#1075 preserve stderr'),
    ('sessions-messages-exit-code', sessions_messages, 'sdk_sessions_messages_exit', 'collect_bounded(child, BOUND).await', '{ let mut output = collect_bounded(child, BOUND).await; output.status = std::process::ExitStatus::from_raw(0); output }', 'fixture_stdin_broken_pipe_preserves_output', '#1075 preserve exit code'),
    ('sessions-messages-stdout', sessions_messages, 'sdk_sessions_messages_exit', 'collect_bounded(child, BOUND).await', '{ let mut output = collect_bounded(child, BOUND).await; output.stdout.clear(); output }', 'fixture_stdin_broken_pipe_preserves_output', '#1075 preserve stdout'),
    ('sessions-messages-stderr', sessions_messages, 'sdk_sessions_messages_exit', 'collect_bounded(child, BOUND).await', '{ let mut output = collect_bounded(child, BOUND).await; output.stderr.clear(); output }', 'fixture_stdin_broken_pipe_preserves_output', '#1075 preserve stderr'),
]


def check(row):
    name, path, target, old, new, test, assertion = row
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
        print(f'#1075 anchors: {len(mutations) - len(errors)}/{len(mutations)} valid')
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

    guarded = [common, sessions_messages, Path(__file__).resolve(),
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
                    path.write_bytes(originals[path])
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
