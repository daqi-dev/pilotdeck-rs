#!/usr/bin/env python3
"""#562 repair 2: count/zip tripwires and real post-hydrate writer state."""
import json
import os
from pathlib import Path
import signal
import subprocess

REPO = Path(__file__).resolve().parents[5]
SRC = REPO / 'src'
assert Path(os.environ['CARGO_TARGET_DIR']).resolve() == SRC / 'target'
OUT = REPO / '_tmp/20261023-issue-222-domain-log/fix2-negative'
OUT.mkdir(parents=True, exist_ok=True)
ADAPTER = SRC / 'pilotdeck-gateway/src/storage/transcript_adapter.rs'
FIXTURES = SRC / 'pilotdeck-gateway/tests/fixtures'
DOMAIN = FIXTURES / 'session-domain/oracle.json'
TRANSCRIPT = FIXTURES / 'transcript-adapter/oracle.json'
originals = {path: path.read_bytes() for path in [ADAPTER, DOMAIN, TRANSCRIPT]}
ADAPTER_TEST = 'storage::transcript_adapter_tests::real_ts_scenarios_match_outcomes_calls_and_bytes'
DOMAIN_TEST = 'storage::session_domain::tests::real_ts_domain_logs_and_events_match_bytes'
results = []


def stop(signum, frame):
    raise SystemExit(f'interrupted by signal {signum}')


signal.signal(signal.SIGTERM, stop)
signal.signal(signal.SIGINT, stop)


def run(name, test, red=False, diagnostic=None):
    slot = subprocess.run(['/Users/da/ws/pilotdeck-rs/scripts/cargo-slots.sh'], timeout=10, capture_output=True, text=True)
    (OUT / f'{name}-slots.log').write_text(slot.stdout + slot.stderr)
    assert slot.returncode == 0, 'no cargo slot; rerun later'
    env = dict(os.environ, CARGO_PROFILE_DEV_DEBUG='0', CARGO_PROFILE_TEST_DEBUG='0', CARGO_INCREMENTAL='0', RUSTC_WRAPPER='', PDRS_BASELINE='/Users/da/ws/pilotdeck-rs/resources/PilotDeck')
    with (OUT / f'{name}.log').open('w') as log:
        child = subprocess.Popen(['cargo', 'test', '-p', 'pilotdeck-gateway', '--lib', test, '--', '--exact', '--nocapture', '--test-threads=4'], cwd=SRC, env=env, stdout=log, stderr=subprocess.STDOUT, start_new_session=True)
        try:
            code = child.wait(timeout=240)
        except BaseException:
            os.killpg(child.pid, signal.SIGKILL)
            child.wait()
            raise
    text = (OUT / f'{name}.log').read_text()
    results.append(dict(name=name, pid=child.pid, exit=code, reaped=child.poll() is not None))
    (OUT / 'processes.json').write_text(json.dumps(results, indent=2) + '\n')
    assert 'running 1 test' in text, f'{name}: compilation/zero-test failure is not evidence'
    if red:
        assert code != 0 and 'test result: FAILED' in text and diagnostic in text, name
    else:
        assert code == 0 and 'test result: ok. 1 passed' in text, name
    print(f'{name}: {"KILLED" if red else "PASS"}; exit={code}; reaped={child.poll() is not None}', flush=True)


try:
    run('baseline-adapter', ADAPTER_TEST)
    run('baseline-domain', DOMAIN_TEST)
    for name, path, test, count in [
        ('drop-adapter-scenarios', TRANSCRIPT, ADAPTER_TEST, 397),
        ('drop-domain-scenarios', DOMAIN, DOMAIN_TEST, 304),
    ]:
        oracle = json.loads(originals[path])
        assert len(oracle['scenarios']) == count
        del oracle['scenarios'][-10:]
        path.write_text(json.dumps(oracle) + '\n')
        run(name, test, True, 'oracle scenario count')
        path.write_bytes(originals[path])
    oracle = json.loads(originals[DOMAIN])
    case = next(case for case in oracle['scenarios'] if case['outcomes'])
    case['outcomes'].pop()
    DOMAIN.write_text(json.dumps(oracle) + '\n')
    run('truncate-domain-outcomes', DOMAIN_TEST, True, 'outcome count')
    DOMAIN.write_bytes(originals[DOMAIN])
    original = originals[ADAPTER].decode()
    start = original.index('    fn restore_entries(')
    end = original.index('    /// JsonlTranscriptWriter.close:', start)
    section = original[start:end]
    block = '''        inner.sequence = state.sequence;
        inner.last_entry_id = state.last_entry_id;
        inner.entry_ids = ids;
        inner.recorder_domain = domain;
'''
    assert section.count(block) == 1
    section = section.replace(block, '').replace('        Ok(())', block + '        Ok(())')
    ADAPTER.write_text(original[:start] + section + original[end:])
    run('install-state-after-hydrate', ADAPTER_TEST, True, 'restore-message-assistant_message-content-string: steps')
    ADAPTER.write_bytes(originals[ADAPTER])
    run('restored-adapter', ADAPTER_TEST)
    run('restored-domain', DOMAIN_TEST)
    print('PASS: 4 / 4 repair-2 mutants killed', flush=True)
finally:
    for path, content in originals.items():
        path.write_bytes(content)
    assert all(path.read_bytes() == content for path, content in originals.items())
