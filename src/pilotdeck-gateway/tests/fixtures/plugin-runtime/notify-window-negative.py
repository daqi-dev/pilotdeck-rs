"""#574: destroy pending callback state and require a bounded test failure.

Run from any directory with the launcher environment and an output directory.
The current noop implementation already passes: restoring noop is no mutation.
This control recreates a callback after notify_waiters has completed, losing its
original Notified state. Compilation errors / parent timeouts never count.
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
from negative_selection import baseline_passed

ROOT = Path(__file__).resolve().parents[5]
OUT = Path(sys.argv[1]).resolve()
OUT.mkdir(parents=True, exist_ok=False)
SOURCE = ROOT / 'src/pilotdeck-gateway/src/runtime/plugins.rs'
TEST = 'runtime::plugins::tests::disposal_notify_between_first_and_spawned_poll_is_retained'
with mutating_sources(ROOT, [SOURCE]) as snapshots:
    before = snapshots[SOURCE]
    old = b'std::task::Poll::Pending => callback.await,'
    new = b'''std::task::Poll::Pending => {
                        drop(callback);
                        match &plugin.dispose {
                            Some(dispose) => dispose().await.map_err(PluginRuntimeError::new),
                            None => Ok(()),
                        }
                    },'''
    assert before.count(old) == 1, 'pending callback anchor must be unique'
    mutated = before.replace(old, new)
    assert mutated != before
    env = dict(os.environ, CARGO_BUILD_JOBS='4')
    target_dir = Path(env.get('CARGO_TARGET_DIR', ROOT / 'src/target')).resolve()
    assert target_dir in {(ROOT / 'src/target').resolve(), (ROOT / '_build/cargo-target').resolve()}, \
        'use this tree src/target or _build/cargo-target'
    started = time.monotonic()
    slot = wait_for_slot(ROOT / 'scripts/cargo-slots.sh', timeout=120)
    (OUT / 'slots.log').write_text(slot.stdout + slot.stderr)
    command = [
        'cargo', 'test', '--offline', '--manifest-path', str(ROOT / 'src/Cargo.toml'),
        '-p', 'pilotdeck-gateway', '--lib',
        TEST,
        '--', '--exact', '--nocapture', '--test-threads=1',
    ]
    # #813 r14: the unmutated bound test must be green first; otherwise a
    # pre-existing failure (load-sensitive deadline, merge fallout) would be
    # misread as this mutation's kill.
    # Without --nocapture: captured output keeps libtest's `test <name> ... ok` line intact.
    baseline = run_owned([arg for arg in command if arg != '--nocapture'],
                         cwd=ROOT, env=env, timeout=240)
    baseline_output = baseline.stdout + baseline.stderr
    (OUT / 'baseline.log').write_text(baseline_output)
    if not (baseline.returncode == 0 and baseline_passed(baseline_output, TEST)):
        (OUT / 'summary.json').write_text(json.dumps(dict(
            sourceSha256=hashlib.sha256(before).hexdigest(), exit=baseline.returncode,
            killed=False, status='INVALID', reason='unmutated bound test is not green',
        ), indent=2) + '\n')
        raise SystemExit('#813 baseline failed: unmutated bound test is not green')
    started = time.monotonic()
    try:
        SOURCE.write_bytes(mutated)
        result = run_owned(command, cwd=ROOT, env=env,
                           timeout=max(1, min(180, 240 - (time.monotonic() - started))))
        output = result.stdout + result.stderr
        (OUT / 'recreate-callback.log').write_text(output)
        # #813: the named test's FAILED line plus the marker inside its own
        # panic block, not ordinary stdout or a sibling test.
        killed = (
            result.returncode == 101
            and named_test_killed(output, TEST,
                                  'disposal lost the notification between first and spawned poll')
            and 'could not compile' not in output
        )
        summary = dict(
            sourceSha256=hashlib.sha256(before).hexdigest(),
            mutationSha256=hashlib.sha256(mutated).hexdigest(),
            exit=result.returncode, killed=killed, seconds=time.monotonic() - started,
        )
        (OUT / 'summary.json').write_text(json.dumps(summary, indent=2) + '\n')
        print(json.dumps(summary), flush=True)
        print(output[-4000:], flush=True)
        assert killed, 'must fail at the test deadline, not compilation or parent timeout'
    finally:
        assert SOURCE.read_bytes() == mutated, 'source changed during exclusive negative control'
        SOURCE.write_bytes(before)
        assert SOURCE.read_bytes() == before
