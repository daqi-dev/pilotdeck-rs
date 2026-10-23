"""#574 repair 3: bounded behavioral controls, one cargo/test process at a time.

Run with the launcher environment and a fresh evidence directory. Restore the
exact source bytes after each mutation; compilation errors never count.
"""
import hashlib
import json
import os
from pathlib import Path
import subprocess
import sys
import time

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_process import run_owned, wait_for_slot

ROOT = Path(__file__).resolve().parents[5]
SOURCE = ROOT / 'src/pilotdeck-gateway/src/runtime/plugins.rs'
OUT = Path(sys.argv[1]).resolve()
OUT.mkdir(parents=True, exist_ok=False)
BASE = SOURCE.read_bytes()
TEXT = BASE.decode()
ENV = dict(os.environ, RUSTC_WRAPPER='', CARGO_BUILD_JOBS='4')
assert Path(ENV.get('CARGO_TARGET_DIR', ROOT / 'src/target')).resolve() == ROOT / 'src/target'
# The exact pushed pre-repair-3 product code, including the repair-2 test hook.
OLD_COMMIT = '6286ed744c4e04ffea76fd7458772316a2364040'
OLD = subprocess.check_output(
    ['git', 'show', f'{OLD_COMMIT}:src/pilotdeck-gateway/src/runtime/plugins.rs'],
    cwd=ROOT, timeout=10,
)

def replace(old, new):
    assert TEXT.count(old) == 1, f'expected one anchor: {old!r}'
    return TEXT.replace(old, new).encode()

# Catch only the eager first poll, allowing an unwind after a Pending poll.
first_only = replace(
    '            let mut callback = Box::pin(std::future::poll_fn(move |cx| {',
    '            let mut caught_first = false;\n'
    '            let mut callback = Box::pin(std::future::poll_fn(move |cx| {\n'
    '                if caught_first { return future.as_mut().poll(cx); }\n'
    '                caught_first = true;',
)
hold_lock = replace(
    '        let mut started = Vec::new();',
    '        let mut started = Vec::new();\n        let _state_guard = state.lock().await;',
)
hold_lock = hold_lock.replace(
    b'        // Every synchronous prologue has run before any callback can resume.',
    b'        drop(_state_guard);\n        // Every synchronous prologue has run before any callback can resume.',
)
CASES = [
    ('restore-old-logic', OLD, 'disposal_sync_panic_does_not_strand_other_waiters',
     'sync panic stranded a waiter: dispose deadline: Elapsed(())'),
    ('catch-first-poll-only', first_only, 'disposal_sync_panic_does_not_strand_other_waiters',
     'sync panic stranded a waiter: dispose deadline: Elapsed(())'),
    ('invoke-under-state-lock', hold_lock, 'disposal_prologue_can_access_runtime_state',
     'DisposeFn invoked under state lock'),
]
summary = []
start = time.monotonic()
try:
    for name, mutation, test, marker in CASES:
        assert mutation != BASE, f'{name} must change bytes'
        assert SOURCE.read_bytes() == BASE, 'source changed outside exclusive runner'
        slot = wait_for_slot('/Users/da/ws/pilotdeck-rs/scripts/cargo-slots.sh',
                             timeout=max(1, 590 - (time.monotonic() - start)))
        (OUT / f'{name}.slots.log').write_text(slot.stdout + slot.stderr)
        try:
            SOURCE.write_bytes(mutation)
            result = run_owned([
                'cargo', 'test', '--offline', '--manifest-path', str(ROOT / 'src/Cargo.toml'),
                '-p', 'pilotdeck-gateway', '--lib', test,
                '--', '--nocapture', '--test-threads=1',
            ], cwd=ROOT, env=ENV, timeout=max(1, min(150, 590 - (time.monotonic() - start))))
            output = result.stdout + result.stderr
            (OUT / f'{name}.log').write_text(output)
            killed = (result.returncode == 101 and marker in output
                      and 'test result: FAILED. 0 passed; 1 failed;' in output
                      and 'could not compile' not in output)
            summary.append(dict(mutation=name, mutationSha256=hashlib.sha256(mutation).hexdigest(),
                                exit=result.returncode, killed=killed, marker=marker))
            print(json.dumps(summary[-1]), flush=True)
            assert killed, 'require behavioral failure, not compile error or parent timeout'
        finally:
            assert SOURCE.read_bytes() == mutation
            SOURCE.write_bytes(BASE)
finally:
    assert SOURCE.read_bytes() == BASE
    (OUT / 'summary.json').write_text(json.dumps(dict(
        sourceSha256=hashlib.sha256(BASE).hexdigest(), restored=True,
        seconds=time.monotonic() - start, cases=summary,
    ), indent=2) + '\n')
