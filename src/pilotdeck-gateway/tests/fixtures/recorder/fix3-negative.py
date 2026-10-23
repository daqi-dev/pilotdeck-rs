#!/usr/bin/env python3
"""#562 repair 3: both recorder count guards reject one missing TS scenario."""
import json
import os
from pathlib import Path
import shutil
import sys

sys.dont_write_bytecode = True
sys.path.insert(0, str(Path(__file__).resolve().parents[1] / '_shared'))
from negative_control import environment, run, scratch

FIXTURES = Path(__file__).resolve().parent
TEST = ['cargo', 'test', '-p', 'pilotdeck-gateway', '--test', 'recorder',
        'real_ts_scenarios_match_outcomes_and_bytes', '--', '--exact',
        '--nocapture', '--test-threads=4']

with scratch('fix3') as root:
    env = environment(root)
    baseline = os.environ['PDRS_BASELINE']
    oracle_path = root / 'oracle.json'
    generator_path = root / 'generate.mts'
    shutil.copyfile(FIXTURES / 'oracle.json', oracle_path)
    shutil.copyfile(FIXTURES / 'generate.mts', generator_path)
    env['PDRS_RECORDER_ORACLE'] = str(oracle_path)
    run('baseline-recorder', TEST, env)
    oracle = json.loads(oracle_path.read_bytes())
    assert len(oracle['scenarios']) == 28
    oracle['scenarios'].pop()
    oracle_path.write_text(json.dumps(oracle) + '\n')
    run('drop-recorder-scenario', TEST, env, diagnostic='recorder oracle scenario count')
    shutil.copyfile(FIXTURES / 'oracle.json', oracle_path)
    source = generator_path.read_text()
    guard = "  assert.equal(scenarios.length, 28, 'recorder oracle scenario count');"
    assert source.count(guard) == 1
    generator_path.write_text(source.replace(guard, '  scenarios.pop(); // negative control\n' + guard))
    run('drop-generated-scenario', ['node', '--import',
        f'{baseline}/node_modules/tsx/dist/loader.mjs', str(generator_path), str(root / 'generated')],
        env, diagnostic='27 !== 28')
    assert not (root / 'generated/oracle.json').exists(), 'failed generator published an oracle'
    run('restored-recorder', TEST, env)
    print('PASS: 2 / 2 repair-3 mutants killed', flush=True)
