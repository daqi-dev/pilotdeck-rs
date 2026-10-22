#!/usr/bin/env python3
"""#518 regression mutations; exclusively own this worktree while running.
All anchors are checked before writes; compile errors/timeouts are not kills.
"""
import hashlib
import json
from pathlib import Path
import subprocess
import sys
import time

ROOT = Path(__file__).resolve().parents[5]
EVIDENCE = Path(sys.argv[1]).resolve()
EVIDENCE.mkdir(parents=True, exist_ok=True)
PRODUCT = ROOT / 'src/pilotdeck-gateway/src/runtime/replacement.rs'
ORACLE = ROOT / 'src/pilotdeck-gateway/tests/fixtures/replacement/oracle.json'
ORIGINAL = {p: p.read_bytes() for p in (PRODUCT, ORACLE)}
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
for name, path, before, after, count, pattern, tests, ignored in MUTATIONS:
    assert ORIGINAL[path].count(before.encode()) == count, name
summary = []
for name, path, before, after, count, pattern, tests, ignored in MUTATIONS:
    while True:
        slots = subprocess.run([str(ROOT / 'scripts/cargo-slots.sh')], capture_output=True, text=True)
        if slots.returncode == 0:
            break
        if slots.returncode != 1:
            raise RuntimeError(slots.stdout + slots.stderr)
        time.sleep(5)
    assert all(p.read_bytes() == data for p, data in ORIGINAL.items()), 'concurrent source edit'
    mutated = ORIGINAL[path].replace(before.encode(), after.encode())
    log = EVIDENCE / f'{name}.log'
    print(f'RUN {name}', flush=True)
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
        assert proc.returncode == 101 and 'could not compile' not in output, (name, str(log))
        for test in tests:
            assert f'test {PREFIX}{test} ... FAILED' in output, (name, test, str(log))
        summary.append({'mutation': name, 'testsFailed': tests, 'exit': proc.returncode,
                        'before': before, 'after': after, 'anchorCount': count,
                        'originalSha256': hashlib.sha256(ORIGINAL[path]).hexdigest()})
        print(f'RED {name}: {len(tests)} targeted failures', flush=True)
    finally:
        assert path.read_bytes() == mutated, f'concurrent edit: {path}'
        path.write_bytes(ORIGINAL[path])
        assert path.read_bytes() == ORIGINAL[path]
        (EVIDENCE / 'summary.json').write_text(json.dumps(summary, ensure_ascii=False, indent=2) + '\n')
print(f'CONFIRMED {len(summary)} compiled mutations; all original bytes restored', flush=True)
