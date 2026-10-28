#!/usr/bin/env python3
# anchor-selfcheck: unsupported #953 Node pin / old-guard migration suite, rows are runtime cases
"""#953 mutation controls for the shared Node pin and old-guard migration.

Run from the repository root after the work is committed (`mutating_sources`
refuses to mutate a product tree that differs from HEAD):

    CARGO_TARGET_DIR=<absolute> PDRS_BASELINE=<fixed checkout> \
        python3 src/pilotdeck-gateway/tests/fixtures/node-provenance/negative.py \
        _tmp/20261026-issue-953/negative-node-provenance

Every row runs its positive control, applies one source mutation, runs the
same bound test, then restores the original bytes and runs the positive control
again. All rows are collected before the summary; any non-KILLED row exits 1.
Cargo always uses the caller's CARGO_TARGET_DIR and never `-q`.
"""
import json
import os
from pathlib import Path
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_process import mutating_sources, run_owned, wait_for_slot

ROOT = Path(__file__).resolve().parents[5]
FIXTURES = 'src/pilotdeck-gateway/tests/fixtures'
NODE_PROVENANCE = 'src/pilotdeck-gateway/tests/common/node_provenance.rs'
GENERATOR = f'{FIXTURES}/edgeclaw-llm/generate.mts'
SOURCE_GUARD = 'src/pilotdeck-gateway/tests/baseline_source_guard.mjs'

ROWS = [
    dict(
        name='wrong-pinned-node',
        path=NODE_PROVENANCE,
        old='"v22.22.0"',
        new='"v22.21.0"',
        runner='cargo',
        test='model_cost::tests::model_cost_fixtures_regenerate_identically_from_ts',
        anchors=['#953 pinned Node runtime'],
    ),
    dict(
        name='old-guard-generator',
        path=GENERATOR,
        old="import { bindBaseline } from '../_shared/generator_guard.mjs';",
        new=(
            "import { bindSourceTree } from '../../baseline_source_guard.mjs';\n"
            "import { bindBaseline } from '../_shared/generator_guard.mjs';"
        ),
        runner='cargo',
        test='fixture_generators_do_not_call_the_old_guard_api',
        anchors=['#953 edgeclaw-llm/generate.mts still imports or calls the old guard API'],
    ),
    dict(
        name='guard-status-diagnostic',
        path=SOURCE_GUARD,
        old="throw Error('baseline has uncommitted changes to tracked files:\\n'+trackedChanges);",
        new="throw Error('baseline tracked changes present');",
        runner='node',
        test='dirty-tracked',
        anchors=['baseline tracked changes present', 'did not match the regular expression'],
    ),
    dict(
        name='guard-hidden-flag-diagnostic',
        path=SOURCE_GUARD,
        old="throw Error('#303 baseline has skip-worktree/assume-unchanged tracked files');",
        new="throw Error('baseline hidden index flags');",
        runner='node',
        test='assume-unchanged',
        anchors=['baseline hidden index flags', 'did not match the regular expression'],
    ),
]


def cargo_command(test):
    return [
        'cargo', 'test', '--offline', '--locked',
        '--manifest-path', str(ROOT / 'src/Cargo.toml'),
        '-p', 'pilotdeck-gateway', '--test', 'regeneration_masks',
        test, '--', '--exact', '--nocapture',
    ]


def cargo_lib_command(test):
    return [
        'cargo', 'test', '--offline', '--locked',
        '--manifest-path', str(ROOT / 'src/Cargo.toml'),
        '-p', 'pilotdeck-gateway', '--lib',
        test, '--', '--ignored', '--exact', '--nocapture',
    ]


def run_cargo(row, env):
    wait_for_slot(ROOT / 'scripts/cargo-slots.sh')
    command = cargo_lib_command(row['test']) if row['test'].startswith('model_cost::') else cargo_command(row['test'])
    return run_owned(command, cwd=ROOT, env=env, timeout=900)


def run_node(row, env):
    node_env = dict(env)
    node_env['PDRS_GIT'] = node_env.get('PDRS_GIT', '/usr/bin/git')
    return run_owned(
        ['node', '--test', f"--test-name-pattern={row['test']}",
         str(ROOT / 'src/pilotdeck-gateway/tests/test_baseline_source_guard.mjs')],
        cwd=ROOT, env=node_env, timeout=300,
    )


def run(row, env):
    return run_cargo(row, env) if row['runner'] == 'cargo' else run_node(row, env)


def cargo_passed(result, test):
    log = result.stdout + result.stderr
    return (result.returncode == 0 and f'test {test} ... ok' in log
            and 'test result: ok. 1 passed;' in log)


def classify(row, result):
    log = result.stdout + result.stderr
    if row['runner'] == 'cargo':
        if cargo_passed(result, row['test']):
            return 'SURVIVED', 'bound test still passed'
        if (result.returncode != 0 and 'test result: FAILED' in log
                and f"test {row['test']} ... FAILED" in log
                and any(anchor in log for anchor in row['anchors'])):
            return 'KILLED', 'bound test failed at its assertion'
        return 'INVALID', 'missing bound runtime failure (build error, wrong panic, or missing test)'
    if result.returncode == 0:
        return 'SURVIVED', 'bound node test still passed'
    if (f'not ok' in log and row['test'] in log
            and any(anchor in log for anchor in row['anchors'])):
        return 'KILLED', 'bound node test failed at its assertion'
    return 'INVALID', 'missing bound node failure (syntax error, wrong test, or missing anchor)'


def positive_ok(row, result):
    if row['runner'] == 'cargo':
        return cargo_passed(result, row['test'])
    return result.returncode == 0


def main():
    output = Path(sys.argv[1]).resolve()
    output.mkdir(parents=True, exist_ok=True)
    if not os.environ.get('CARGO_TARGET_DIR'):
        raise SystemExit('CARGO_TARGET_DIR must be supplied by the caller')
    if not os.environ.get('PDRS_BASELINE'):
        raise SystemExit('PDRS_BASELINE must be supplied by the caller')
    env = dict(os.environ, CARGO_NET_OFFLINE='true')
    paths = [row['path'] for row in ROWS]
    results = []
    try:
        with mutating_sources(ROOT, paths) as originals:
            for row in ROWS:
                status, reason = 'INVALID', ''
                try:
                    base = run(row, env)
                    if not positive_ok(row, base):
                        status, reason = 'INVALID', 'positive control did not pass the bound test'
                    else:
                        original = originals[row['path']].decode()
                        if original.count(row['old']) != 1:
                            status, reason = 'INVALID', 'mutation anchor is not unique'
                        else:
                            Path(ROOT / row['path']).write_bytes(
                                original.replace(row['old'], row['new'], 1).encode())
                            status, reason = classify(row, run(row, env))
                except Exception as error:
                    status, reason = 'INVALID', f'{type(error).__name__}: {error}'
                finally:
                    Path(ROOT / row['path']).write_bytes(originals[row['path']])
                try:
                    restored = run(row, env)
                    if not positive_ok(row, restored):
                        status, reason = 'INVALID', (reason + '; ' if reason else '') + 'restored positive control did not pass'
                except Exception as error:
                    status, reason = 'INVALID', (reason + '; ' if reason else '') + f'restored: {type(error).__name__}: {error}'
                results.append(dict(name=row['name'], test=row['test'], status=status, reason=reason))
                print(f"{row['name']}: {status}: {reason}", flush=True)
    except Exception as error:
        reason = f'source guard: {type(error).__name__}: {error}'
        for row in ROWS[len(results):]:
            results.append(dict(name=row['name'], test=row['test'], status='INVALID', reason=reason))
    counts = {state: sum(row['status'] == state for row in results)
              for state in ('KILLED', 'SURVIVED', 'INVALID')}
    (output / 'summary.json').write_text(json.dumps(
        {'controls': results, 'counts': counts}, indent=2) + '\n')
    print('summary: ' + ', '.join(f'{state}={count}' for state, count in counts.items()))
    return int(not results or any(row['status'] != 'KILLED' for row in results))


if __name__ == '__main__':
    raise SystemExit(main())
