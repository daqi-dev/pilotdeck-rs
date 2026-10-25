#!/usr/bin/env python3
"""#518 regression mutations; exclusively own this worktree while running.
All anchors are checked before writes; compile errors/timeouts are not kills.
"""
import hashlib
import json
import os
from pathlib import Path
import subprocess
import sys
import time

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_process import mutating_sources

ROOT = Path(__file__).resolve().parents[5]
PRODUCT = ROOT / 'src/pilotdeck-gateway/src/runtime/replacement.rs'
ORACLE = ROOT / 'src/pilotdeck-gateway/tests/fixtures/replacement/oracle.json'
PREFIX = 'runtime::replacement::tests::'
PANICS = [f'panic_{path}_{mode}' for mode in ('sync', 'poll') for path in (
    'replace_abort', 'replace_close', 'replace_replace', 'rollback_close',
    'rollback_finalize', 'commit', 'timeout_close', 'timeout_finalize', 'fork')]
PANICS += ['panic_active_getter', 'panic_finalize_getter',
           'panic_timeout_getter_rearms_without_poisoning_state',
           'panic_disposed_dropped_replace', 'panic_disposed_dropped_finalize',
           'panic_disposed_dropped_commit']
MUTATIONS = [
    ('remove_panic_boundary', PRODUCT, 'catch_unwind(AssertUnwindSafe(call)).map_err',
     'Ok::<T, Box<dyn std::any::Any + Send>>(call()).map_err', 1, 'panic_', PANICS, False),
    ('secondary_waiter_panic', PRODUCT,
     'task.await\n            .unwrap_or_else(|error| Err(ReplacementError::new(error.to_string())))',
     'task.await.expect("replacement operation panicked")', 1,
     'unexpected_task_panic_returns_error_to_waiter', ['unexpected_task_panic_returns_error_to_waiter'], False),
    ('omit_success_clear_by_key', PRODUCT,
     'this.0.state.lock().unwrap().pending.remove(&key);',
     'this.0.state.lock().unwrap().pending.get(&key);', 3,
     'stale_', [f'stale_{kind}_success_clears_new_pending' for kind in ('finalize', 'commit', 'timeout')], False),
    ('omit_stale_replace_overwrite', PRODUCT,
     'state.pending.insert(key.clone(), pending);',
     'state.pending.entry(key.clone()).or_insert(pending);', 1,
     'stale_replace_success_overwrites_new_pending', ['stale_replace_success_overwrites_new_pending'], False),
    ('omit_stale_replace_failure_clear', PRODUCT,
     'if result.is_err() {\n                state.pending.remove(&key);\n            }',
     'if result.is_err() {\n                state.pending.get(&key);\n            }', 1,
     'stale_replace_failure_clears_new_pending', ['stale_replace_failure_clears_new_pending'], False),
    ('abandoned_claim_not_submitting', PRODUCT,
     'pending.phase = Phase::Submitting;\n        cancel_timer(pending);',
     'pending.phase = Phase::Prepared;\n        cancel_timer(pending);', 1,
     'abandoned_submit_claim', ['abandoned_submit_claim'], False),
    ('corrupt_regenerated_observation', ORACLE,
     '"timerCount": 0', '"timerCount": 1', 1,
     'replacement_fixture_regenerates_identically', ['replacement_fixture_regenerates_identically'], True),
]
MUTATIONS += [
    ('remove_warning_boundary', PRODUCT,
     'let _ = port_call(|| (self.0.warn)(message, error));',
     '(self.0.warn)(message, error);', 1,
     'warning_panic_', ['warning_panic_commit_cleanup', 'warning_panic_timeout_cleanup', 'warning_panic_getter_cleanup'], False),
    ('omit_getter_panic_rearm', PRODUCT,
     'Err(error) => {\n                    self.schedule_locked(&mut state, &key);',
     'Err(error) => {', 1,
     'panic_timeout_getter_rearms_without_poisoning_state',
     ['panic_timeout_getter_rearms_without_poisoning_state'], False),
]
# A failed test alone is insufficient: dependency/host failures are INVALID.
FAILURE_TEXT = {
    'remove_panic_boundary': ['results differ from TS'],
    'secondary_waiter_panic': ['replacement operation panicked: JoinError::Panic'],
    'omit_success_clear_by_key': [f'stale_{kind}_success_clears_new_pending: results differ from TS'
                                 for kind in ('finalize', 'commit', 'timeout')],
    'omit_stale_replace_overwrite': ['stale_replace_success_overwrites_new_pending: results differ from TS'],
    'omit_stale_replace_failure_clear': ['stale_replace_failure_clears_new_pending: results differ from TS'],
    'abandoned_claim_not_submitting': ['abandoned_submit_claim: results differ from TS'],
    'corrupt_regenerated_observation': ['replacement/oracle.json differs from the regenerated'],
    'remove_warning_boundary': ['warning panic skipped commit cleanup', 'warning panic stranded timeout phase'],
    'omit_getter_panic_rearm': ['getter panic retry did not finish rollback'],
}


def invalid_reason(name, tests, returncode, output):
    if returncode != 101 or 'could not compile' in output:
        return 'test did not compile and fail with exit 101'
    missing = [f'test {PREFIX}{test} ... FAILED' for test in tests
               if f'test {PREFIX}{test} ... FAILED' not in output]
    missing += [text for text in FAILURE_TEXT[name] if text not in output]
    return 'missing expected failure: ' + '; '.join(missing) if missing else None


def main():
    # Reject before any mutation, cargo invocation or evidence-directory creation.
    if not os.environ.get('PDRS_BASELINE'):
        print('INVALID: set PDRS_BASELINE before running #518 mutations', file=sys.stderr)
        return 2
    evidence = Path(sys.argv[1]).resolve()
    evidence.mkdir(parents=True, exist_ok=True)
    with mutating_sources(ROOT, [PRODUCT, ORACLE]) as snapshots:
        original = {p: snapshots[p] for p in (PRODUCT, ORACLE)}
        for name, path, before, after, count, pattern, tests, ignored in MUTATIONS:
            assert original[path].count(before.encode()) == count, name
            assert FAILURE_TEXT[name], name
        summary = []
        for name, path, before, after, count, pattern, tests, ignored in MUTATIONS:
            while True:
                slots = subprocess.run([str(ROOT / 'scripts/cargo-slots.sh')], capture_output=True, text=True)
                if slots.returncode == 0:
                    break
                if slots.returncode != 1:
                    raise RuntimeError(slots.stdout + slots.stderr)
                time.sleep(5)
            assert all(p.read_bytes() == data for p, data in original.items()), 'concurrent source edit'
            mutated = original[path].replace(before.encode(), after.encode())
            log = evidence / f'{name}.log'
            print(f'RUN {name}', flush=True)
            record = {'mutation': name, 'status': 'INVALID', 'before': before, 'after': after,
                      'anchorCount': count, 'expectedFailureText': FAILURE_TEXT[name],
                      'originalSha256': hashlib.sha256(original[path]).hexdigest()}
            try:
                path.write_bytes(mutated)
                command = ['cargo', 'test', '--offline', '-p', 'pilotdeck-gateway', '--lib', PREFIX + pattern, '--']
                if ignored:
                    command.append('--ignored')
                with log.open('w') as output:
                    output.write(slots.stdout + slots.stderr)
                    output.flush()
                    proc = subprocess.run(command, cwd=ROOT / 'src', stdout=output,
                                          stderr=subprocess.STDOUT, timeout=300)
                output = log.read_text(errors='replace')
                reason = invalid_reason(name, tests, proc.returncode, output)
                record['exit'] = proc.returncode
                if reason:
                    record['reason'] = reason
                    print(f'INVALID {name}: {reason}; {log}', flush=True)
                else:
                    record.update(status='CONFIRMED', testsFailed=tests)
                    print(f'RED {name}: {len(tests)} targeted failures with expected diagnostics', flush=True)
            except subprocess.TimeoutExpired:
                record['reason'] = 'test timed out'
                print(f'INVALID {name}: test timed out; {log}', flush=True)
            finally:
                assert path.read_bytes() == mutated, f'concurrent edit: {path}'
                path.write_bytes(original[path])
                assert path.read_bytes() == original[path]
                summary.append(record)
                (evidence / 'summary.json').write_text(json.dumps(summary, ensure_ascii=False, indent=2) + '\n')
            if record['status'] != 'CONFIRMED':
                return 1
        print(f'CONFIRMED {len(summary)} compiled mutations; all original bytes restored', flush=True)
        return 0


if __name__ == '__main__':
    sys.exit(main())
