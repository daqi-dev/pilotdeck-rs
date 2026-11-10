#!/usr/bin/env python3
"""#2351: runner panic must close the drain notification with a false verdict.

Run --check before committing, then NEW_OUTPUT_DIR after committing. The shared
matrix executes positive -> mutant --no-run -> negative -> restored positive
for every row, verifies nonce receipts, and summarizes all KILLED/SURVIVED/INVALID
results. CARGO_TARGET_DIR must be supplied by the caller. The HEAD guard is never
bypassed: both source and this fixture must be committed before mutation runs.
"""
from pathlib import Path
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
# matrix_main uses these shared primitives for every mutation/build; keep the
# ownership contract explicit here instead of adding a second execution loop.
from negative_process import mutating_sources, run_owned, wait_for_slot
from negative_suite import matrix_main

root = Path(__file__).resolve().parents[5]
source = root / 'src/pilotdeck-gateway/src/native_ws.rs'
tests = root / 'src/pilotdeck-gateway/src/native_ws/tests.rs'
cases = [
    (
        'sender-outlives-runner', source,
        '        let (runner_done, done) = watch::channel(None::<bool>);',
        '        let (runner_done, done) = watch::channel(None::<bool>);\n'
        '        std::mem::forget(runner_done.clone());',
        'phased_drain_runner_panic_wakes_all_waiters',
        '#2351 panic wakes every drained waiter within deadline',
    ),
    (
        'runner-failure-as-success', source,
        '            if receiver.changed().await.is_err() {\n'
        '                return false;\n'
        '            }',
        '            if receiver.changed().await.is_err() {\n'
        '                return true;\n'
        '            }',
        'phased_drain_runner_panic_wakes_all_waiters',
        '#2351 panic is a failed drain for every waiter',
    ),
]

if __name__ == '__main__':
    raise SystemExit(matrix_main(
        root, __file__, 'native_ws::tests::', cases,
        passed_count=1, child_count=0, paths=[source, tests], exact=True,
    ))
