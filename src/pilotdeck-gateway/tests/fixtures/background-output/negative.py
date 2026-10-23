#!/usr/bin/env python3
"""Bounded serial source mutations. Restores exact bytes in finally; one target only."""
import json
import os
from pathlib import Path
import signal
import subprocess
import sys
import time

root = Path(__file__).resolve().parents[5]
output = Path(sys.argv[1]).resolve()
output.mkdir(parents=True, exist_ok=True)
background = root / 'src/pilotdeck-gateway/src/runtime/background.rs'
store = root / 'src/pilotdeck-gateway/src/runtime/background/output.rs'
original = {p: p.read_bytes() for p in (background, store)}
mutations = [
    ('threshold-minus-one', background, 'const DEFAULT_MEMORY_BYTES: usize = 1_000_000;', 'const DEFAULT_MEMORY_BYTES: usize = 999_999;'),
    ('threshold-plus-one', background, 'const DEFAULT_MEMORY_BYTES: usize = 1_000_000;', 'const DEFAULT_MEMORY_BYTES: usize = 1_000_001;'),
    ('read-order', store, 'content = disk;', 'content = disk.into_iter().rev().collect();'),
    ('unconfigured-spill', store, 'spill: path.map(|path| Spill {', 'spill: path.or_else(|| Some(std::env::temp_dir().join("pdrs-553-negative-unconfigured.log"))).map(|path| Spill {'),
    ('overflow-only-spill', store, 'spill.append(chunk.to_vec());', 'if self.memory + chunk.len() > self.max { spill.append(chunk.to_vec()); }'),
    ('truncate-existing-log', store, '.append(true)', '.write(true).truncate(true)'),
    ('skip-flush', store, 'let _ = tail.wait_for(Option::is_some).await;', 'let _ = tail;'),
    ('retry-disabled', store, 'fs::create_dir_all(path.parent().expect("spill parent"))?;', 'if !path.parent().expect("spill parent").exists() { return Err(std::io::Error::from_raw_os_error(libc::EACCES)); }'),
    ('failed-write-marked-ready', store, 'write().is_ok()', '{ let _ = write(); true }'),
    ('gap-unreported', store, 'truncated = true;', 'truncated = false;'),
    ('runtime-layout', background, 'dir.join("output")', 'dir.join("wrong-output")'),
    ('closed-accepts-output', store, 'if self.closed || chunk.is_empty() {', 'if chunk.is_empty() {'),
]
if len(sys.argv) > 2:
    requested = set(sys.argv[2:])
    mutations = [mutation for mutation in mutations if mutation[0] in requested]
    assert {mutation[0] for mutation in mutations} == requested, 'unknown mutation'
env = dict(os.environ, RUSTC_WRAPPER='', RUST_TEST_THREADS='4')
command = ['cargo','test','-p','pilotdeck-gateway','--lib','spill_replays_real_ts_oracle','--','--test-threads=4']
signal.signal(signal.SIGTERM, lambda *_: (_ for _ in ()).throw(KeyboardInterrupt()))
started = time.monotonic()
results = []
def run(label):
    remaining = 590 - (time.monotonic() - started)
    if remaining <= 0:
        raise RuntimeError('negative-control total 10 minute budget exhausted')
    with (output / (label+'.log')).open('w') as log:
        selected = ['cargo','test','-p','pilotdeck-gateway','--test','background_output','--','--test-threads=4'] if label == 'runtime-layout' else command
        process = subprocess.Popen(selected, cwd=root/'src', env=env, stdout=log, stderr=subprocess.STDOUT, start_new_session=True)
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
    (Path(os.environ.get('TMPDIR','/tmp'))/'pdrs-553-negative-unconfigured.log').unlink(missing_ok=True)
    (output/'summary.json').write_text(json.dumps({'elapsedSeconds':round(time.monotonic()-started,2),'mutations':results},indent=2)+'\n')
code, _ = run('restored')
assert code == 0, 'restored implementation must pass'
