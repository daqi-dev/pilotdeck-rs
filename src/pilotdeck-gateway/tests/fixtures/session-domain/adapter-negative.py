#!/usr/bin/env python3
"""Full gateway mutation checks for store ordering and the former Node gaps."""
import os
from pathlib import Path
import signal
import subprocess

REPO = Path(__file__).resolve().parents[5]
SRC = REPO / 'src'
assert Path(os.environ['CARGO_TARGET_DIR']).resolve() == SRC / 'target'
OUT = REPO / '_tmp/20261023-issue-222-domain-log/adapter-negative'
OUT.mkdir(parents=True, exist_ok=True)
ADAPTER = SRC / 'pilotdeck-gateway/src/storage/transcript_adapter.rs'
RECORDER = SRC / 'pilotdeck-gateway/src/storage/recorder.rs'
TEST = SRC / 'pilotdeck-gateway/src/storage/transcript_adapter_tests.rs'
originals = {p: p.read_text() for p in [ADAPTER, RECORDER, TEST]}

def stop(signum, frame):
    raise SystemExit(f'interrupted by signal {signum}')
signal.signal(signal.SIGTERM, stop)
signal.signal(signal.SIGINT, stop)

def restore():
    for path, text in originals.items():
        path.write_text(text)

def run(name, red):
    slot = subprocess.run(['/Users/da/ws/pilotdeck-rs/scripts/cargo-slots.sh'], timeout=10, capture_output=True, text=True)
    (OUT / f'{name}-slots.log').write_text(slot.stdout + slot.stderr)
    assert slot.returncode == 0, 'no cargo slot; rerun later'
    env = dict(os.environ, CARGO_PROFILE_DEV_DEBUG='0', CARGO_PROFILE_TEST_DEBUG='0')
    with (OUT / f'{name}.log').open('w') as log:
        child = subprocess.Popen(['cargo', 'test', '-p', 'pilotdeck-gateway', '--lib', 'storage::transcript_adapter_tests::real_ts_scenarios_match_outcomes_calls_and_bytes', '--', '--exact', '--nocapture'], cwd=SRC, env=env, stdout=log, stderr=subprocess.STDOUT, start_new_session=True)
        try:
            code = child.wait(timeout=240)
        except BaseException:
            os.killpg(child.pid, signal.SIGKILL)
            child.wait()
            raise
    text = (OUT / f'{name}.log').read_text()
    assert 'running 1 test' in text, f'{name}: compilation/zero-test failure is not evidence'
    assert ('test result: FAILED' in text and code != 0) if red else ('test result: ok. 1 passed' in text and code == 0), name
    print(f'{name}: {"KILLED" if red else "PASS"}', flush=True)

try:
    run('baseline', False)
    # Restore the actual removed NodeHostRequired variant and its turn refusal.
    for path, text in originals.items():
        path.write_text(text.replace('ProjectionUnavailable', 'NodeHostRequired'))
    text = ADAPTER.read_text()
    anchor = '        domain.next(entry).map_err(AdapterError::from)'
    assert text.count(anchor) == 1
    ADAPTER.write_text(text.replace(anchor, '''        if field(entry, "type").as_string().is_some_and(|t| t.equals_str("turn_started")) {
            return Err(AdapterError::NodeHostRequired("Session domain validation for turn_started entries requires the Node host.".into()));
        }
''' + anchor))
    run('restore-node-host-required', True)
    restore()
    before = originals[ADAPTER]
    changes = [
      ('premature-malformed-restore-refusal', '        // Hydration runs every registered reducer.', '        for entry in entries { projection_supported(entry)?; }\n        // Hydration runs every registered reducer.'),
      ('list-null-result-refusal', 'return Err(AdapterError::Type(format!(\n                    "Cannot read properties of {} (reading \'usage\')"', 'return Err(AdapterError::ProjectionUnavailable(format!(\n                    "Cannot read properties of {} (reading \'usage\')"'),
      ('adapter-skips-domain-validation', anchor, '        Ok(domain.clone())'),
      ('domain-published-before-store', '        self.base\n            .store\n            .append(&self.base.key(&self.path), &entry)', '        inner.recorder_domain = domain.clone();\n        self.base\n            .store\n            .append(&self.base.key(&self.path), &entry)'),
    ]
    for name, old, new in changes:
        assert before.count(old) == 1, (name, before.count(old))
        ADAPTER.write_text(before.replace(old, new))
        run(name, True)
        restore()
    a = before.index('    fn restore_entries(')
    b = before.index('    /// JsonlTranscriptWriter.close', a)
    segment = before[a:b]
    assert segment.count('inner.recorder_domain = domain;') == 1
    ADAPTER.write_text(before[:a] + segment.replace('inner.recorder_domain = domain;', 'inner.recorder_domain = DomainState::default();') + before[b:])
    run('restore-loses-domain-state', True)
    restore()
    run('restored', False)
    print('PASS: 6 / 6 adapter mutants killed', flush=True)
finally:
    restore()
