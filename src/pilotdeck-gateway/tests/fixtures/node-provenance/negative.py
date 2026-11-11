#!/usr/bin/env python3
# anchor-selfcheck: unsupported #953 Node pin / old-guard migration suite, rows are runtime cases
"""#953 mutation controls for the shared Node pin and old-guard migration.

Run from the repository root after the work is committed (`mutating_sources`
refuses to mutate a product tree that differs from HEAD;
``NEGATIVE_BASELINE=<commit>`` rehearses against a snapshot of the
uncommitted tree, the same escape the edgeclaw suites use):

    CARGO_TARGET_DIR=<absolute> PDRS_BASELINE=<fixed checkout> \
        python3 src/pilotdeck-gateway/tests/fixtures/node-provenance/negative.py \
        _tmp/20261026-issue-953/negative-node-provenance-<attempt>

Every row runs its positive control, applies one source mutation, runs the
same bound test, then restores the original bytes and runs the positive control
again. All rows are collected before the summary; any non-KILLED row exits 1.
Cargo always uses the caller's CARGO_TARGET_DIR and never `-q`.

The output directory must be missing or empty: reusing one that already holds
evidence would overwrite the previous run's original logs. Every log is created
exclusively, so a rerun needs a new directory.
"""
import json
import os
from pathlib import Path
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_process import (exclusive_write, mutating_sources,
                              new_output_directory, run_owned, wait_for_slot,
                              write_source_atomic)
from negative_panic import named_test_killed
from negative_suite import invalidate_completed, passed

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


# No `--nocapture` in either command: it splices the test's stdout into the
# `test <name> ... ok/FAILED` status line (#938 r14) that the shared
# passed()/named_test_killed() judge exactly (#1053, #1087).
def cargo_command(test):
    return [
        'cargo', 'test', '--offline', '--locked',
        '--manifest-path', str(ROOT / 'src/Cargo.toml'),
        '-p', 'pilotdeck-gateway', '--test', 'regeneration_masks',
        test, '--', '--exact',
    ]


def cargo_lib_command(test):
    return [
        'cargo', 'test', '--offline', '--locked',
        '--manifest-path', str(ROOT / 'src/Cargo.toml'),
        '-p', 'pilotdeck-gateway', '--lib',
        test, '--', '--ignored', '--exact',
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


def run(row, env, output, stage):
    result = run_cargo(row, env) if row['runner'] == 'cargo' else run_node(row, env)
    exclusive_write(output / f"{row['name']}-{stage}.log", result.stdout + result.stderr)
    return result


def _tap_test_name(not_ok_line):
    text = not_ok_line[len('not ok '):]
    return text.split(' - ', 1)[1] if ' - ' in text else text


def failing_subtests(log):
    """(name, block) for every failing ``node --test`` subtest (#1087).

    Node's TAP output puts a failing subtest's assertion message inside the
    indented ``---``/``...`` block that follows its ``not ok N - <name>`` line,
    while anything a test prints to stdout lands on unindented top-level
    ``# comment`` lines. A marker therefore only counts when it is inside the
    bound subtest's own failing block — the node-side form of #818's "stdout
    is not a panic payload".
    """
    entries = []
    state = {'name': None, 'block': None}

    def close():
        if state['block'] is not None:
            entries.append((state['name'], '\n'.join(state['block'])))
            state['block'] = None

    for line in log.split('\n'):
        if line.startswith('not ok ') or line.startswith('ok '):
            close()
            if line.startswith('not ok '):
                state['name'] = _tap_test_name(line)
                state['block'] = []
        elif line == '  ...':
            close()
        elif state['block'] is not None and (not line or line.startswith('  ')):
            state['block'].append(line)
        elif line and not line.startswith('  '):
            close()
    close()
    return entries


def node_test_killed(log, test, anchors):
    """The bound subtest failed and a marker is inside its own block."""
    return any(name == test and any(anchor in block for anchor in anchors)
               for name, block in failing_subtests(log))


def classify(row, result):
    log = result.stdout + result.stderr
    if row['runner'] == 'cargo':
        if passed(result, row['test']):
            return 'SURVIVED', 'bound test still passed'
        # #1087: only the named test's own FAILED status line plus a marker in
        # that test's own panic payload kills (negative_panic judges both).
        if result.returncode != 0 and named_test_killed(log, row['test'], row['anchors']):
            return 'KILLED', 'bound test failed at its assertion'
        return 'INVALID', 'missing bound runtime failure (build error, wrong panic, or missing test)'
    if result.returncode == 0:
        return 'SURVIVED', 'bound node test still passed'
    if node_test_killed(log, row['test'], row['anchors']):
        return 'KILLED', 'bound node test failed at its assertion'
    return 'INVALID', 'missing bound node failure (syntax error, wrong test, or missing anchor)'


def positive_ok(row, result):
    if row['runner'] == 'cargo':
        return passed(result, row['test'])
    return result.returncode == 0


def main():
    if not os.environ.get('CARGO_TARGET_DIR'):
        raise SystemExit('CARGO_TARGET_DIR must be supplied by the caller')
    if not os.environ.get('PDRS_BASELINE'):
        raise SystemExit('PDRS_BASELINE must be supplied by the caller')
    output = new_output_directory(sys.argv[1])
    env = dict(os.environ, CARGO_NET_OFFLINE='true')
    paths = [row['path'] for row in ROWS]
    results = []
    try:
        with mutating_sources(ROOT, paths,
                              baseline=os.environ.get('NEGATIVE_BASELINE') or 'HEAD') as originals:
            for row in ROWS:
                status, reason = 'INVALID', ''
                try:
                    base = run(row, env, output, 'positive')
                    if not positive_ok(row, base):
                        status, reason = 'INVALID', 'positive control did not pass the bound test'
                    else:
                        snapshot = originals[Path(row['path'])]
                        original = snapshot.decode()
                        if original.count(row['old']) != 1:
                            status, reason = 'INVALID', 'mutation anchor is not unique'
                        else:
                            # #1087 fix-r2: every guarded-source write in the
                            # window is one atomic rename (#1903 rule, shared
                            # with ``mutating_sources``) — a truncating
                            # ``write_bytes`` killed mid-write leaves an empty
                            # or half-written product source.
                            write_source_atomic(
                                ROOT, ROOT / row['path'],
                                original.replace(row['old'], row['new'], 1).encode(),
                                mode=(ROOT / row['path']).stat().st_mode & 0o7777)
                            status, reason = classify(row, run(row, env, output, 'negative'))
                except Exception as error:
                    status, reason = 'INVALID', f'{type(error).__name__}: {error}'
                finally:
                    # Same #1087 fix-r2 window: the in-loop restore is also one
                    # rename, never a truncating write_bytes.
                    write_source_atomic(
                        ROOT, ROOT / row['path'], originals[Path(row['path'])],
                        mode=(ROOT / row['path']).stat().st_mode & 0o7777)
                try:
                    restored = run(row, env, output, 'restored')
                    if not positive_ok(row, restored):
                        status, reason = 'INVALID', (reason + '; ' if reason else '') + 'restored positive control did not pass'
                except Exception as error:
                    status, reason = 'INVALID', (reason + '; ' if reason else '') + f'restored: {type(error).__name__}: {error}'
                results.append(dict(name=row['name'], test=row['test'], status=status, reason=reason))
                print(f"{row['name']}: {status}: {reason}", flush=True)
    except Exception as error:
        reason = f'source guard: {type(error).__name__}: {error}'
        invalidate_completed(results, reason)
        for row in ROWS[len(results):]:
            results.append(dict(name=row['name'], test=row['test'], status='INVALID', reason=reason))
    counts = {state: sum(row['status'] == state for row in results)
              for state in ('KILLED', 'SURVIVED', 'INVALID')}
    exclusive_write(output / 'summary.json', json.dumps(
        {'controls': results, 'counts': counts}, indent=2) + '\n')
    print('summary: ' + ', '.join(f'{state}={count}' for state, count in counts.items()))
    return int(not results or any(row['status'] != 'KILLED' for row in results))


if __name__ == '__main__':
    raise SystemExit(main())
