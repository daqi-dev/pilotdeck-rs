# anchor-selfcheck: unsupported CASES contains precomputed complete mutated files, not before fragments
"""#574 repair 3: bounded behavioral controls, one cargo/test process at a time.

Run with the launcher environment and a fresh evidence directory. Restore the
exact source bytes after each mutation; compilation errors never count.
Optional mutation ids (``restore-old-logic``, ``catch-first-poll-only``,
``invoke-under-state-lock``) select a subset; every one still runs in its own
process, after an unmutated baseline for each selected target test.
"""
import hashlib
import json
import os
from pathlib import Path
import sys
import time

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_panic import named_test_failed, named_test_killed
from negative_process import mutating_sources, run_owned, wait_for_slot
from negative_selection import baseline_passed, select

ROOT = Path(__file__).resolve().parents[5]
TARGET = ROOT / '_build/cargo-target'
assert Path(os.environ.get('CARGO_TARGET_DIR', TARGET)).resolve() == TARGET.resolve(), '#1166 use this worktree _build/cargo-target only'
ENV = dict(os.environ, CARGO_TARGET_DIR=str(TARGET), RUSTC_WRAPPER='', CARGO_BUILD_JOBS='4')
SOURCE = ROOT / 'src/pilotdeck-gateway/src/runtime/plugins.rs'
OUT = Path(sys.argv[1]).resolve()
OUT.mkdir(parents=True, exist_ok=False)
with mutating_sources(ROOT, [SOURCE]) as snapshots:
    BASE = snapshots[SOURCE]
    TEXT = BASE.decode()

    def replace(old, new):
        assert TEXT.count(old) == 1, f'expected one anchor: {old!r}'
        return TEXT.replace(old, new).encode()

    # Pre-repair-3 behaviour on the current source: a dispose panic is not
    # converted into a disposal error, so the start task unwinds and the rest of
    # the batch is stranded. Replaying the whole 6286ed7 file no longer compiles
    # against the current plugins_tests.rs (later changes added fields its tests
    # use); this reverse diff keeps the same behavioural claim.
    old_logic = replace(
        '                let mut callback = Box::pin(std::future::poll_fn(move |cx| {\n'
        '                    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {\n'
        '                        future.as_mut().poll(cx)\n'
        '                    }))\n'
        '                    .unwrap_or_else(|payload| {\n'
        '                        let message = crate::runtime::cleanup_task::panic_message(payload.as_ref())\n'
        '                            .unwrap_or("Plugin dispose callback panicked.");\n'
        '                        std::task::Poll::Ready(Err(PluginRuntimeError::new(message)))\n'
        '                    })\n'
        '                }));',
        '                let mut callback = Box::pin(std::future::poll_fn(move |cx| {\n'
        '                    future.as_mut().poll(cx)\n'
        '                }));',
    )

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
    # Both the timeout expectation and the completion assertion are real
    # detections of the stranding. Neither is a generic `assert_eq!` message:
    # the completion anchor is asserted in plugins_tests.rs (#813 fix-r4).
    STRANDED_MARKERS = (
        'sync panic stranded a waiter: dispose deadline',
        'sync panic stranded a waiter: completion count',
    )
    # Catching only the first poll does not strand the batch: the later-poll
    # panic unwinds the spawned disposal task, which reports the task-drop
    # message instead of the callback's own panic message (host run 0558b389,
    # plugins_tests.rs `released.message` assertion).
    FIRST_POLL_MARKER = 'left: "Plugin disposal task was dropped or panicked before completion."'
    CASES = [
        ('restore-old-logic', old_logic, 'disposal_sync_panic_does_not_strand_other_waiters',
         STRANDED_MARKERS),
        ('catch-first-poll-only', first_only, 'disposal_sync_panic_does_not_strand_other_waiters',
         FIRST_POLL_MARKER),
        ('invoke-under-state-lock', hold_lock, 'disposal_prologue_can_access_runtime_state',
         'DisposeFn invoked under state lock'),
    ]
    # Optional mutation ids keep a host re-run bounded; `always=()` because the
    # unmutated baseline is run explicitly below, before any mutation.
    cases = select(CASES, sys.argv[2:], always=())
    # #813: the libtest status line and the panic block carry the full module
    # path; the bare function name cannot be judged exactly.
    TEST_PREFIX = 'runtime::plugins::tests::'
    baseline_tests = list(dict.fromkeys(test for _name, _mutation, test, _marker in cases))
    summary = []
    count = {'KILLED': 0, 'SURVIVED': 0, 'INVALID': 0, 'PASS': 0}
    start = time.monotonic()

    def record(row):
        summary.append(row)
        count[row['status']] += 1
        print(json.dumps(row), flush=True)

    try:
        # #813 fix-r4: a subset run must first prove the selected target tests
        # are green without a mutation. Otherwise a pre-existing failure would
        # be attributed to the mutant and count as KILLED. A failed baseline
        # invalidates only its dependent mutants; every other mutant still runs
        # so one host round reports every problem (#813 fix-r12).
        failed_baselines = set()
        for test in baseline_tests:
            test_name = TEST_PREFIX + test
            passed, exit_code, detail = False, None, ''
            try:
                assert SOURCE.read_bytes() == BASE, 'source changed outside exclusive runner'
                slot = wait_for_slot(ROOT / 'scripts/cargo-slots.sh',
                                     timeout=max(1, 590 - (time.monotonic() - start)))
                (OUT / f'baseline-{test}.slots.log').write_text(slot.stdout + slot.stderr)
                result = run_owned([
                    'cargo', 'test', '--offline', '--manifest-path', str(ROOT / 'src/Cargo.toml'),
                    '-p', 'pilotdeck-gateway', '--lib', test_name,
                    '--', '--exact', '--test-threads=1',
                ], cwd=ROOT, env=ENV, timeout=max(1, min(150, 590 - (time.monotonic() - start))))
                exit_code = result.returncode
                output = result.stdout + result.stderr
                (OUT / f'baseline-{test}.log').write_text(output)
                passed = (result.returncode == 0 and baseline_passed(output, test_name))
                detail = 'unmutated target test is green' if passed else 'baseline failed'
            except Exception as error:
                detail = f'baseline: {type(error).__name__}: {error}'
            if not passed:
                failed_baselines.add(test_name)
            record(dict(mutation='baseline', test=test_name, exit=exit_code,
                        baseline=passed, status='PASS' if passed else 'INVALID',
                        reason=detail))
        for name, mutation, test, marker in cases:
            test_name = TEST_PREFIX + test
            status, reason, exit_code, mutated = 'INVALID', '', None, False
            if test_name in failed_baselines:
                record(dict(mutation=name, mutationSha256=hashlib.sha256(mutation).hexdigest(),
                            exit=None, test=test_name, marker=marker, status='INVALID',
                            reason='baseline failed; mutant verdict would be ambiguous'))
                continue
            try:
                assert mutation != BASE, f'{name} must change bytes'
                assert SOURCE.read_bytes() == BASE, 'source changed outside exclusive runner'
                slot = wait_for_slot(ROOT / 'scripts/cargo-slots.sh',
                                     timeout=max(1, 590 - (time.monotonic() - start)))
                (OUT / f'{name}.slots.log').write_text(slot.stdout + slot.stderr)
                SOURCE.write_bytes(mutation)
                mutated = True
                result = run_owned([
                    'cargo', 'test', '--offline', '--manifest-path', str(ROOT / 'src/Cargo.toml'),
                    '-p', 'pilotdeck-gateway', '--lib', test_name,
                    '--', '--exact', '--test-threads=1',
                ], cwd=ROOT, env=ENV, timeout=max(1, min(150, 590 - (time.monotonic() - start))))
                exit_code = result.returncode
                output = result.stdout + result.stderr
                (OUT / f'{name}.log').write_text(output)
                # #813: `test {test_name} ... FAILED` plus the marker inside that
                # test's panic block, never ordinary stdout or a sibling test.
                if result.returncode == 0 and baseline_passed(output, test_name):
                    status, reason = 'SURVIVED', 'bound test still passed'
                elif (result.returncode == 101
                      and named_test_killed(output, test_name, marker)
                      and 'could not compile' not in output):
                    status, reason = 'KILLED', 'bound test failed at its assertion'
                elif named_test_failed(output, test_name):
                    status, reason = 'INVALID', 'bound test failed without the bound assertion'
                elif 'could not compile' in output:
                    status, reason = 'INVALID', 'mutant did not compile'
                else:
                    status, reason = 'INVALID', f'bound test did not fail (exit {result.returncode})'
            except Exception as error:
                status, reason = 'INVALID', f'{type(error).__name__}: {error}'
            finally:
                if mutated:
                    if SOURCE.read_bytes() == mutation:
                        SOURCE.write_bytes(BASE)
                    else:
                        status, reason = 'INVALID', (
                            reason + '; mutation bytes changed outside runner').lstrip('; ')
            record(dict(mutation=name, mutationSha256=hashlib.sha256(mutation).hexdigest(),
                        exit=exit_code, test=test_name, marker=marker,
                        status=status, reason=reason))
    finally:
        assert SOURCE.read_bytes() == BASE
        (OUT / 'summary.json').write_text(json.dumps(dict(
            sourceSha256=hashlib.sha256(BASE).hexdigest(), restored=True,
            counts=count,
            invalid=count['INVALID'] > 0,
            seconds=time.monotonic() - start, cases=summary,
        ), indent=2) + '\n')
    raise SystemExit(int(count['INVALID'] > 0 or count['SURVIVED'] > 0))
