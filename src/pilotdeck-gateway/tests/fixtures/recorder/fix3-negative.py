#!/usr/bin/env python3
"""#562 repair 3: both recorder count guards reject one missing TS scenario."""
import json
import os
from pathlib import Path
import signal
import subprocess
import time

REPO = Path(__file__).resolve().parents[5]
SRC = REPO / 'src'
assert Path(os.environ['CARGO_TARGET_DIR']).resolve() == SRC / 'target'
OUT = REPO / '_tmp/20261023-issue-222-domain-log/fix3-negative'
OUT.mkdir(parents=True, exist_ok=True)
FIXTURES = Path(__file__).resolve().parent
ORACLE = FIXTURES / 'oracle.json'
GENERATOR = FIXTURES / 'generate.mts'
originals = {path: path.read_bytes() for path in [ORACLE, GENERATOR]}
env = dict(os.environ, CARGO_PROFILE_DEV_DEBUG='0', CARGO_PROFILE_TEST_DEBUG='0',
           CARGO_INCREMENTAL='0', RUSTC_WRAPPER='')
baseline = os.environ['PDRS_BASELINE']
TEST = ['cargo', 'test', '-p', 'pilotdeck-gateway', '--test', 'recorder',
        'real_ts_scenarios_match_outcomes_and_bytes', '--', '--exact',
        '--nocapture', '--test-threads=4']
results = []


def stop(signum, frame):
    raise SystemExit(f'interrupted by signal {signum}')


signal.signal(signal.SIGTERM, stop)
signal.signal(signal.SIGINT, stop)


def run(name, command, red=False):
    if command[0] == 'cargo':
        deadline = time.monotonic() + 120
        while True:
            slot = subprocess.run(['/Users/da/ws/pilotdeck-rs/scripts/cargo-slots.sh'],
                                  capture_output=True, text=True, timeout=10)
            (OUT / f'{name}-slots.log').write_text(slot.stdout + slot.stderr)
            if slot.returncode == 0:
                break
            assert slot.returncode == 1 and time.monotonic() < deadline, 'cargo slots full'
            time.sleep(10)
    with (OUT / f'{name}.log').open('w') as log:
        child = subprocess.Popen(command, cwd=SRC, env=env, stdout=log,
                                 stderr=subprocess.STDOUT, start_new_session=True)
        try:
            code = child.wait(timeout=240)
        except BaseException:
            os.killpg(child.pid, signal.SIGKILL)
            child.wait()
            raise
    result = dict(name=name, pid=child.pid, exit=code, reaped=child.poll() is not None)
    results.append(result)
    (OUT / 'processes.json').write_text(json.dumps(results, indent=2) + '\n')
    text = (OUT / f'{name}.log').read_text()
    if command[0] == 'cargo':
        assert 'running 1 test' in text, 'compilation or zero-test failure is not evidence'
    if red:
        assert code != 0 and 'recorder oracle scenario count' in text, name
        if command[0] == 'cargo':
            assert 'test result: FAILED' in text and 'left: 27' in text and 'right: 28' in text
        else:
            assert '27 !== 28' in text
    else:
        assert code == 0 and 'test result: ok. 1 passed' in text, name
    print(f'{name}: {"KILLED" if red else "PASS"}; exit={code}; reaped=True', flush=True)


try:
    run('baseline-recorder', TEST)
    oracle = json.loads(originals[ORACLE])
    assert len(oracle['scenarios']) == 28
    oracle['scenarios'].pop()
    ORACLE.write_text(json.dumps(oracle) + '\n')
    run('drop-recorder-scenario', TEST, True)
    ORACLE.write_bytes(originals[ORACLE])
    source = originals[GENERATOR].decode()
    guard = "  assert.equal(scenarios.length, 28, 'recorder oracle scenario count');"
    assert source.count(guard) == 1
    GENERATOR.write_text(source.replace(guard, '  scenarios.pop(); // negative control\n' + guard))
    run('drop-generated-scenario', ['node', '--import',
        f'{baseline}/node_modules/tsx/dist/loader.mjs', str(GENERATOR), str(OUT / 'generated')], True)
    GENERATOR.write_bytes(originals[GENERATOR])
    assert not (OUT / 'generated/oracle.json').exists(), 'failed generator published an oracle'
    run('restored-recorder', TEST)
    print('PASS: 2 / 2 repair-3 mutants killed', flush=True)
finally:
    for path, content in originals.items():
        path.write_bytes(content)
    assert all(path.read_bytes() == content for path, content in originals.items())
