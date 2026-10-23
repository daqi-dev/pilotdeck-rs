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
from negative_process import run_owned, wait_for_slot

ROOT = Path(__file__).resolve().parents[5]
OUT = Path(sys.argv[1]).resolve()
OUT.mkdir(parents=True, exist_ok=False)
SOURCE = ROOT / 'src/pilotdeck-gateway/src/runtime/plugins.rs'
before = SOURCE.read_bytes()
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
assert Path(env.get('CARGO_TARGET_DIR', ROOT / 'src/target')).resolve() == ROOT / 'src/target'
started = time.monotonic()
slot = wait_for_slot('/Users/da/ws/pilotdeck-rs/scripts/cargo-slots.sh', timeout=120)
(OUT / 'slots.log').write_text(slot.stdout + slot.stderr)
try:
    SOURCE.write_bytes(mutated)
    result = run_owned([
        'cargo', 'test', '--offline', '--manifest-path', str(ROOT / 'src/Cargo.toml'),
        '-p', 'pilotdeck-gateway', '--lib',
        'disposal_notify_between_first_and_spawned_poll_is_retained',
        '--', '--nocapture', '--test-threads=1',
    ], cwd=ROOT, env=env, timeout=max(1, min(180, 240 - (time.monotonic() - started))))
    output = result.stdout + result.stderr
    (OUT / 'recreate-callback.log').write_text(output)
    killed = (
        result.returncode == 101
        and 'disposal lost the notification between first and spawned poll: Elapsed(())' in output
        and 'test result: FAILED. 0 passed; 1 failed;' in output
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
