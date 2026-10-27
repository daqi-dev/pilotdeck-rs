#!/usr/bin/env python3
"""#926: commit first; never bypass the source == HEAD guard.

CARGO_TARGET_DIR=<this tree's target> python3 negative_controls.py NEW_OUT [names...]
Each selected case runs positive, mutant no-run, mutant test, restored test.
Every case gets KILLED / SURVIVED / INVALID; stale anchors are INVALID, never kills.
"""
import json
import os
from pathlib import Path
import subprocess
import sys

root = Path(__file__).resolve().parents[5]
sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_process import mutating_sources, run_owned, wait_for_slot
from negative_suite import Case, classify, passed

source = root / 'src/pilotdeck-gateway/tests/common/oracle_locale.rs'
mutations = [
    ('stdout-no-renewal', source,
     'out.extend_from_slice(&out_chunk[..count]);\n                        deadline.as_mut().reset(tokio::time::Instant::now() + idle_limit);',
     'out.extend_from_slice(&out_chunk[..count]);',
     'stdout_bytes_renew_idle_deadline', '#926 progress: bytes renew idle deadline'),
    ('stderr-no-renewal', source,
     'err.extend_from_slice(&err_chunk[..count]);\n                        deadline.as_mut().reset(tokio::time::Instant::now() + idle_limit);',
     'err.extend_from_slice(&err_chunk[..count]);',
     'stderr_bytes_renew_idle_deadline', '#926 progress: bytes renew idle deadline'),
    ('stdout-eof-renews', source,
     'out_open = count != 0;\n                    if count != 0 {',
     'out_open = count != 0;\n                    if true {',
     'eof_does_not_renew_deadline_or_complete_a_live_child', '#926 EOF: EOF must not renew deadline'),
    ('stderr-eof-renews', source,
     'err_open = count != 0;\n                    if count != 0 {',
     'err_open = count != 0;\n                    if true {',
     'eof_does_not_renew_deadline_or_complete_a_live_child', '#926 EOF: EOF must not renew deadline'),
    ('late-idle-timeout', source,
     'let deadline = tokio::time::sleep(idle_limit);',
     'let deadline = tokio::time::sleep(idle_limit + idle_limit);',
     'silent_child_expires_at_idle_deadline', '#926 silent: must expire at idle deadline'),
    ('timer-before-ready-output', source,
     'tokio::select! {\n                biased;',
     'if deadline.is_elapsed() { return Err(io::Error::new(io::ErrorKind::TimedOut, "mutant timer first")); }\n            tokio::select! {\n                biased;',
     'ready_output_and_exit_win_over_overdue_timer', '#926 ready: queued output precedes deadline'),
    ('timer-before-ready-exit', source,
     'exited = &mut wait, if status.is_none() => {',
     'exited = &mut wait, if status.is_none() && !deadline.is_elapsed() => {',
     'ready_output_and_exit_win_over_overdue_timer', '#926 ready: queued exit precedes deadline'),
    ('skip-tail-drain', source,
     'while status.is_none() || out_open || err_open {',
     'while status.is_none() {',
     'exited_child_still_drains_both_pipes_with_idle_bound', '#926 drain: exit alone is not complete'),
    ('discard-stdout', source,
     'out.extend_from_slice(&out_chunk[..count]);',
     'let _ = &mut out;',
     'stdout_bytes_renew_idle_deadline', '#926 progress: stdout retained'),
    ('discard-stderr', source,
     'err.extend_from_slice(&err_chunk[..count]);',
     'let _ = &mut err;',
     'stderr_bytes_renew_idle_deadline', '#926 progress: stderr retained'),
    ('swallow-read-error', source,
     'read = stdout.read(&mut out_chunk), if out_open => {\n                    let count = read?;',
     'read = stdout.read(&mut out_chunk), if out_open => {\n                    let count = read.unwrap_or(0);',
     'read_and_wait_errors_are_not_success', '#926 error: read reason retained'),
    ('swallow-wait-error', source,
     'status = Some(exited?);',
     'status = Some(exited.unwrap_or_else(|_| std::os::unix::process::ExitStatusExt::from_raw(0)));',
     'read_and_wait_errors_are_not_success', '#926 error: wait must fail'),
    ('skip-timeout-kill-reap', source,
     'if result.is_err() {', 'if false && result.is_err() {',
     'idle_timeout_kills_and_reaps_child', '#926 cleanup: timeout reaps before returning'),
    ('skip-cancel-kill', source,
     '.kill_on_drop(true)', '.kill_on_drop(false)',
     'cancelling_supervision_kills_child', '#926 cleanup: child must be killed and reaped'),
]


def main():
    if not os.environ.get('CARGO_TARGET_DIR'):
        raise SystemExit('CARGO_TARGET_DIR required; no implicit/shared target')
    out = Path(sys.argv[1]).resolve()
    out.mkdir(parents=True, exist_ok=False)
    selected = sys.argv[2:]
    unknown = set(selected) - {row[0] for row in mutations}
    if unknown:
        raise SystemExit(f'unknown mutations: {sorted(unknown)}')
    env = os.environ.copy()
    env.update(CARGO_PROFILE_DEV_DEBUG='line-tables-only', CARGO_PROFILE_TEST_DEBUG='line-tables-only')
    command = ['cargo', 'test', '-q', '--offline', '--manifest-path', 'src/Cargo.toml', '-p', 'pilotdeck-gateway', '--lib']
    summary = []

    for name, path, old, new, test_name, marker in mutations:
        if selected and name not in selected:
            continue
        test = 'test_oracle_locale::tests::' + test_name
        case = Case(name, path, old, new, test, marker)
        status, reason = 'INVALID', 'not run'

        def run(label, args):
            slot = wait_for_slot(root / 'scripts/cargo-slots.sh', timeout=180)
            (out / f'{name}.{label}.slots.log').write_text(slot.stdout + slot.stderr)
            try:
                result = run_owned(args, cwd=root, env=env, timeout=300)
            except subprocess.TimeoutExpired as error:
                raw = ''.join(v.decode(errors='replace') if isinstance(v, bytes) else v or '' for v in (error.stdout, error.stderr))
                (out / f'{name}.{label}.log').write_text(raw + f'\n{error}\n')
                raise
            (out / f'{name}.{label}.log').write_text(result.stdout + result.stderr)
            return result

        args = command + [test, '--', '--exact', '--nocapture', '--format', 'pretty']
        entered = False
        try:
            with mutating_sources(root, [path]) as originals:
                entered = True
                original = originals[path].decode()
                count = original.count(old)
                if count != 1:
                    raise ValueError(f'stale anchor: expected 1, got {count}')
                if not passed(run('positive', args), test):
                    raise ValueError('bound positive test did not pass')
                path.write_text(original.replace(old, new, 1))
                compile_result = run('compile', command + ['--no-run'])
                if compile_result.returncode:
                    raise ValueError(f'mutant compilation failed rc={compile_result.returncode}')
                status, reason = classify(run('negative', args), case)
        except Exception as error:
            status, reason = 'INVALID', str(error)
        # The official context manager has restored and verified HEAD bytes.
        # Do not test an unexpected original if the source guard refused entry.
        if entered:
            try:
                if not passed(run('restored', args), test):
                    status, reason = 'INVALID', reason + '; restored bound test did not pass'
            except Exception as error:
                status, reason = 'INVALID', reason + f'; restored: {error}'
        summary.append(dict(mutation=name, test=test, assertion=marker, status=status, reason=reason))
        print(f'{name}: {status}: {reason}', flush=True)
        (out / 'summary.json').write_text(json.dumps(summary, indent=2, ensure_ascii=False) + '\n')
    counts = {state: sum(row['status'] == state for row in summary) for state in ('KILLED', 'SURVIVED', 'INVALID')}
    print(json.dumps(counts), flush=True)
    return int(not summary or any(row['status'] != 'KILLED' for row in summary))


if __name__ == '__main__':
    sys.exit(main())
