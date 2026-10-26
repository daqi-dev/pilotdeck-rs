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
from negative_panic import named_test_killed
from negative_process import mutating_sources, run_owned, wait_for_slot
from negative_selection import baseline_passed, select

ROOT = Path(__file__).resolve().parents[5]
SOURCE = ROOT / 'src/pilotdeck-gateway/src/runtime/plugins.rs'
OUT = Path(sys.argv[1]).resolve()
OUT.mkdir(parents=True, exist_ok=False)
with mutating_sources(ROOT, [SOURCE]) as snapshots:
    BASE = snapshots[SOURCE]
    TEXT = BASE.decode()
    ENV = dict(os.environ, RUSTC_WRAPPER='', CARGO_BUILD_JOBS='4')
    target_dir = Path(ENV.get('CARGO_TARGET_DIR', ROOT / 'src/target')).resolve()
    assert target_dir in {(ROOT / 'src/target').resolve(), (ROOT / '_build/cargo-target').resolve()}, \
        'use this tree src/target or _build/cargo-target'

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
        '                        let message = payload\n'
        '                            .downcast_ref::<String>()\n'
        '                            .cloned()\n'
        '                            .or_else(|| payload.downcast_ref::<&str>().map(|s| (*s).to_owned()))\n'
        '                            .unwrap_or_else(|| "Plugin dispose callback panicked.".to_owned());\n'
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
    CASES = [
        ('restore-old-logic', old_logic, 'disposal_sync_panic_does_not_strand_other_waiters',
         STRANDED_MARKERS),
        ('catch-first-poll-only', first_only, 'disposal_sync_panic_does_not_strand_other_waiters',
         STRANDED_MARKERS),
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
    start = time.monotonic()
    try:
        # #813 fix-r4: a subset run must first prove the selected target tests
        # are green without a mutation. Otherwise a pre-existing failure would
        # be attributed to the mutant and count as KILLED.
        for test in baseline_tests:
            test_name = TEST_PREFIX + test
            assert SOURCE.read_bytes() == BASE, 'source changed outside exclusive runner'
            slot = wait_for_slot(ROOT / 'scripts/cargo-slots.sh',
                                 timeout=max(1, 590 - (time.monotonic() - start)))
            (OUT / f'baseline-{test}.slots.log').write_text(slot.stdout + slot.stderr)
            result = run_owned([
                'cargo', 'test', '--offline', '--manifest-path', str(ROOT / 'src/Cargo.toml'),
                '-p', 'pilotdeck-gateway', '--lib', test_name,
                '--', '--exact', '--nocapture', '--test-threads=1',
            ], cwd=ROOT, env=ENV, timeout=max(1, min(150, 590 - (time.monotonic() - start))))
            output = result.stdout + result.stderr
            (OUT / f'baseline-{test}.log').write_text(output)
            passed = (result.returncode == 0 and baseline_passed(output, test_name))
            summary.append(dict(mutation='baseline', test=test_name, exit=result.returncode,
                                killed=False, baseline=passed))
            print(json.dumps(summary[-1]), flush=True)
            if not passed:
                raise SystemExit(f'baseline {test_name} failed: INVALID before mutation')
        for name, mutation, test, marker in cases:
            test_name = TEST_PREFIX + test
            assert mutation != BASE, f'{name} must change bytes'
            assert SOURCE.read_bytes() == BASE, 'source changed outside exclusive runner'
            slot = wait_for_slot(ROOT / 'scripts/cargo-slots.sh',
                                 timeout=max(1, 590 - (time.monotonic() - start)))
            (OUT / f'{name}.slots.log').write_text(slot.stdout + slot.stderr)
            try:
                SOURCE.write_bytes(mutation)
                result = run_owned([
                    'cargo', 'test', '--offline', '--manifest-path', str(ROOT / 'src/Cargo.toml'),
                    '-p', 'pilotdeck-gateway', '--lib', test_name,
                    '--', '--exact', '--nocapture', '--test-threads=1',
                ], cwd=ROOT, env=ENV, timeout=max(1, min(150, 590 - (time.monotonic() - start))))
                output = result.stdout + result.stderr
                (OUT / f'{name}.log').write_text(output)
                # #813: `test {test_name} ... FAILED` plus the marker inside that
                # test's panic block, never ordinary stdout or a sibling test.
                killed = (result.returncode == 101
                          and named_test_killed(output, test_name, marker)
                          and 'could not compile' not in output)
                summary.append(dict(mutation=name, mutationSha256=hashlib.sha256(mutation).hexdigest(),
                                    exit=result.returncode, test=test_name, killed=killed, marker=marker))
                print(json.dumps(summary[-1]), flush=True)
                assert killed, 'require behavioral failure, not compile error or parent timeout'
            finally:
                assert SOURCE.read_bytes() == mutation
                SOURCE.write_bytes(BASE)
    finally:
        assert SOURCE.read_bytes() == BASE
        (OUT / 'summary.json').write_text(json.dumps(dict(
            sourceSha256=hashlib.sha256(BASE).hexdigest(), restored=True,
            invalid=any(row.get('baseline') is False for row in summary),
            seconds=time.monotonic() - start, cases=summary,
        ), indent=2) + '\n')
