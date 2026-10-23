#!/usr/bin/env python3
"""#562 repair 5: both count guards reject added and missing TS scenarios."""
import copy
import json
import os
from pathlib import Path
import shutil
import sys

sys.dont_write_bytecode = True
sys.path.insert(0, str(Path(__file__).resolve().parents[1] / '_shared'))
from negative_control import environment, run, scratch

FIXTURES = Path(__file__).resolve().parent
TEST = ['cargo', 'test', '-p', 'pilotdeck-gateway', '--lib',
        'storage::run_registry_tests::real_ts_scenarios_match_outcomes_and_snapshot_bytes',
        '--', '--exact', '--nocapture', '--test-threads=4']

with scratch('run-registry-count') as root:
    env = environment(root)
    baseline = os.environ['PDRS_BASELINE']
    oracle_path = root / 'oracle.json'
    original = (FIXTURES / 'oracle.json').read_bytes()
    oracle_path.write_bytes(original)
    env['PDRS_RUN_REGISTRY_ORACLE'] = str(oracle_path)
    run('baseline-run-registry', TEST, env)
    for name, change in [('drop', -1), ('add', 1)]:
        oracle = json.loads(original)
        assert len(oracle['scenarios']) == 25
        if change < 0:
            oracle['scenarios'].pop()
        else:
            oracle['scenarios'].append(copy.deepcopy(oracle['scenarios'][-1]))
        oracle_path.write_text(json.dumps(oracle) + '\n')
        run(f'{name}-run-registry-scenario', TEST, env,
            diagnostic='run-registry oracle scenario count')
    oracle_path.write_bytes(original)

    # Keep the relative provenance import valid without touching the source tree.
    shared = root / '_shared'
    shared.mkdir()
    shutil.copyfile(FIXTURES.parent / '_shared/baseline.mts', shared / 'baseline.mts')
    generator_path = root / 'run-registry/generate.mts'
    generator_path.parent.mkdir()
    source = (FIXTURES / 'generate.mts').read_text()
    guard = "  assert.equal(scenarios.length, 25, 'run-registry oracle scenario count');"
    assert source.count(guard) == 1
    for name, mutation, actual in [
        ('drop', '  scenarios.pop();', 24),
        ('add', '  scenarios.push(structuredClone(scenarios.at(-1)));', 26),
    ]:
        generator_path.write_text(source.replace(guard, mutation + '\n' + guard))
        output = root / f'{name}-generated'
        run(f'{name}-generated-run-registry-scenario', ['node', '--import',
            f'{baseline}/node_modules/tsx/dist/loader.mjs', str(generator_path), str(output)],
            env, diagnostic=f'{actual} !== 25')
        assert not (output / 'oracle.json').exists(), 'failed generator published an oracle'
    generator_path.write_text(source)
    run('restored-run-registry', TEST, env)
    print('PASS: 4 / 4 run-registry count mutants killed', flush=True)
