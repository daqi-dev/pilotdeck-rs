#!/usr/bin/env python3
"""Bounded serial mutations in a disposable source copy; one existing target only."""
import json
import os
from pathlib import Path
import shutil
import signal
import subprocess
import sys
import time
import tempfile

root = Path(__file__).resolve().parents[5]
output = Path(sys.argv[1]).resolve()
output.mkdir(parents=True, exist_ok=True)
# No source mutation ever touches the real worktree, even if this script is killed.
scratch = tempfile.TemporaryDirectory(prefix='pdrs-553-negative-')
copy = Path(scratch.name)
shutil.copytree(root / 'src', copy / 'src',
                ignore=shutil.ignore_patterns('target', 'node_modules', '__pycache__'))
background = copy / 'src/pilotdeck-gateway/src/runtime/background.rs'
store = copy / 'src/pilotdeck-gateway/src/runtime/background/output.rs'
original = {p: p.read_bytes() for p in (background, store)}
mutations = [
    ('threshold-minus-one', background, 'const DEFAULT_MEMORY_BYTES: usize = 1_000_000;', 'const DEFAULT_MEMORY_BYTES: usize = 999_999;'),
    ('threshold-plus-one', background, 'const DEFAULT_MEMORY_BYTES: usize = 1_000_000;', 'const DEFAULT_MEMORY_BYTES: usize = 1_000_001;'),
    ('read-order', store, 'content = disk;', 'content = disk.into_iter().rev().collect();'),
    ('unconfigured-spill', store, 'spill: path.map(|path| Spill {', 'spill: path.or_else(|| Some(std::path::PathBuf::from(std::env::var_os("PDRS_553_NEGATIVE_SPILL_PATH").unwrap()))).map(|path| Spill {'),
    ('overflow-only-spill', store, 'spill.append(chunk.to_vec());', 'if self.memory + chunk.len() > self.max { spill.append(chunk.to_vec()); }'),
    ('truncate-existing-log', store, '.append(true)', '.write(true).truncate(true)'),
    ('skip-flush', store, 'let _ = tail.wait_for(Option::is_some).await;', 'let _ = tail;'),
    ('retry-disabled', store, 'fs::create_dir_all(path.parent().expect("spill parent"))?;', 'if !path.parent().expect("spill parent").exists() { return Err(std::io::Error::from_raw_os_error(libc::EACCES)); }'),
    ('failed-write-marked-ready', store, 'write().is_ok()', '{ let _ = write(); true }'),
    ('gap-unreported', store, 'truncated = true;', 'truncated = false;'),
    ('runtime-layout', background, 'dir.join("output")', 'dir.join("wrong-output")'),
    ('x-dispose-no-flush-await', background, '                flushed.await;', '                drop(flushed);'),
    ('x-begin-close-not-closing', store, 'self.closed = true;\n        self.flush()', 'self.flush()'),
    ('utf8-disk-decoded-before-ring', store, 'content = disk;', 'content = String::from_utf8_lossy(&disk).into_owned().into_bytes();'),
    ('disk-cap-falls-through', store, 'if remaining <= 0.0 {\n            return slice', 'if remaining < 0.0 {\n            return slice'),
    ('closed-disk-unreadable', store, 'let path = &self.spill.as_ref()?.path;', 'if self.closed { return None; } let path = &self.spill.as_ref()?.path;'),
    ('ring-leading-replacement-lost', store, 'String::from_utf8_lossy(&content).into_owned()', "if truncated { String::from_utf8_lossy(&content).trim_start_matches('\\u{fffd}').to_owned() } else { String::from_utf8_lossy(&content).into_owned() }"),
    ('closed-accepts-output', store, 'if self.closed || chunk.is_empty() {', 'if chunk.is_empty() {'),
]
if len(sys.argv) > 2:
    requested = set(sys.argv[2:])
    mutations = [mutation for mutation in mutations if mutation[0] in requested]
    assert {mutation[0] for mutation in mutations} == requested, 'unknown mutation'
env = dict(os.environ, RUSTC_WRAPPER='', RUST_TEST_THREADS='4')
# The disposable copy shares the launcher's sole target, never creates another.
target = Path(env.get('CARGO_TARGET_DIR', root / 'src/target')).resolve()
assert target == (root / 'src/target').resolve(), f'unexpected target: {target}'
env['CARGO_TARGET_DIR'] = str(target)
# Keep this one mutant file in our scratch even after SIGKILL.
env['PDRS_553_NEGATIVE_SPILL_PATH'] = str(copy / 'pdrs-553-negative-unconfigured.log')
slots = root / 'scripts/cargo-slots.sh'
command = ['cargo','test','-p','pilotdeck-gateway','--lib','runtime::background::','--','--test-threads=4']
signal.signal(signal.SIGTERM, lambda *_: (_ for _ in ()).throw(KeyboardInterrupt()))
started = time.monotonic()
results = []
def run(label):
    remaining = 590 - (time.monotonic() - started)
    if remaining <= 0:
        raise RuntimeError('negative-control total 10 minute budget exhausted')
    # Never treat an occupied slot as permission to start another Cargo.
    while True:
        check = subprocess.run([str(slots)], capture_output=True, text=True, timeout=5)
        (output / 'slots.log').open('a').write(check.stdout + check.stderr)
        if check.returncode == 0:
            break
        if check.returncode != 1 or time.monotonic() - started >= 590:
            raise RuntimeError('cargo slots unavailable within the total budget')
        time.sleep(min(2, remaining))
        remaining = 590 - (time.monotonic() - started)
    with (output / (label+'.log')).open('w') as log:
        selected = ['cargo','test','-p','pilotdeck-gateway','--test','background_output','--','--test-threads=4'] if label in ('runtime-layout', 'baseline-runtime', 'restored-runtime') else command
        if label in ('utf8-disk-decoded-before-ring', 'disk-cap-falls-through', 'closed-disk-unreadable', 'ring-leading-replacement-lost'):
            selected = [arg if arg != 'runtime::background::' else 'spill_replays_added_ts_boundary_cases' for arg in command]
        process = subprocess.Popen(selected, cwd=copy/'src', env=env, stdout=log, stderr=subprocess.STDOUT, start_new_session=True)
        try:
            code = process.wait(timeout=min(100,remaining))
        except BaseException:
            os.killpg(process.pid,signal.SIGKILL)
            process.wait()
            raise
    text = (output / (label+'.log')).read_text()
    if 'error[E' in text or 'could not compile' in text or 'Operation not permitted' in text:
        raise RuntimeError(f'{label}: infrastructure/compile error is not a killed mutation')
    return code, text
try:
    code, _ = run('baseline')
    assert code == 0, 'baseline must pass'
    if any(m[0] == 'runtime-layout' for m in mutations):
        code, _ = run('baseline-runtime')
        assert code == 0, 'runtime baseline must pass'
    for name, path, before, after in mutations:
        source = original[path].decode()
        assert source.count(before) == 1, (name, source.count(before))
        path.write_text(source.replace(before, after))
        code, log = run(name)
        assert code != 0 and 'test result: FAILED' in log, f'{name}: mutation survived or did not reach test'
        results.append({'name':name,'exit':code,'killed':True})
        path.write_bytes(original[path])
        print(f'{name}: killed (exit {code})',flush=True)
finally:
    for path, content in original.items():
        path.write_bytes(content)
    # Only this script owns this exact synthetic mutant path.
    (copy/'pdrs-553-negative-unconfigured.log').unlink(missing_ok=True)
    (output/'summary.json').write_text(json.dumps({'elapsedSeconds':round(time.monotonic()-started,2),'mutations':results},indent=2)+'\n')
try:
    code, _ = run('restored')
    assert code == 0, 'restored implementation must pass'
    if any(m[0] == 'runtime-layout' for m in mutations):
        code, _ = run('restored-runtime')
        assert code == 0, 'restored runtime must pass'
finally:
    scratch.cleanup()
