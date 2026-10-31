#!/usr/bin/env python3
# anchor-selfcheck: unsupported imperative replacement chain, no mutation table
"""#1139 recorder count guards reject missing and added TS scenarios.

#280 moved recorder/generate.mts onto the shared provenance guard, so the old
single-file copy died on ERR_MODULE_NOT_FOUND before the count assertion and
never judged the mutant. Like run-registry count-negative (#1129 fix-r2/r3),
copy the generator's whole literal local import closure with the repository
layout; a missing closure file or one escaping the repository is an error.

Run after committing, with caller-owned CARGO_TARGET_DIR and PDRS_BASELINE.
Run exclusively in this worktree: no concurrent builds, tests or source
writers. The source guard intentionally refuses uncommitted sources.
"""
from contextlib import contextmanager
import copy
import json
import os
from pathlib import Path
import re
import shutil
import sys

sys.dont_write_bytecode = True
sys.path.insert(0, str(Path(__file__).resolve().parents[1] / '_shared'))
from negative_control import environment, scratch
from negative_process import mutating_sources, run_owned, wait_for_slot
from negative_suite import Case, classify, passed

FIXTURES = Path(__file__).resolve().parent
REPO = FIXTURES.parents[4]
TEST_NAME = 'real_ts_scenarios_match_outcomes_and_bytes'
TEST = ['cargo', 'test', '-p', 'pilotdeck-gateway', '--test', 'recorder',
        TEST_NAME, '--', '--exact', '--nocapture', '--test-threads=4']


def local_import_closure(entry, source_root):
    """Find literal relative ESM imports, retaining their repository layout.

    Like baseline_source_guard.mjs, scan from/import string specifiers (including
    re-exports, side-effect imports and literal dynamic imports). This fixture
    closure uses exact filenames; missing files are errors, never skipped.
    """
    source_root = source_root.resolve()
    pending = [entry]
    seen = set()
    while pending:
        path = pending.pop().resolve()
        if not path.is_relative_to(source_root):
            raise ValueError(f'#1139 local import escapes source root: {path}')
        if path in seen:
            continue
        if not path.is_file():
            raise FileNotFoundError(f'#1139 missing local import: {path}')
        seen.add(path)
        text = path.read_text()
        for match in re.finditer(r'''(?:\bfrom\s*|\bimport\s*\(\s*|\bimport\s+)(['"])([^'"]+)\1''', text):
            specifier = match[2]
            if specifier.startswith(('./', '../')):
                pending.append(path.parent / specifier)
    return sorted(seen)


def copy_local_import_closure(entry, source_root, destination_root):
    for path in local_import_closure(entry, source_root):
        target = destination_root / path.relative_to(source_root.resolve())
        target.parent.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(path, target)
    return destination_root / entry.resolve().relative_to(source_root.resolve())


@contextmanager
def guarded_inputs():
    fixture_root = FIXTURES.parent
    generator = FIXTURES / 'generate.mts'
    guarded = [generator, FIXTURES / 'oracle.json', Path(__file__).resolve(),
               fixture_root / '_shared/negative_control.py',
               *(fixture_root / name for name in (
                   'negative_process.py', 'negative_suite.py',
                   'negative_panic.py', 'pending_reanchor.py',
               ))]
    # Discover imports only after the entry and runner are checked under the
    # tree lock; keep that same reentrant lock through copying and all controls.
    with mutating_sources(REPO, guarded):
        with mutating_sources(REPO, local_import_closure(generator, REPO)):
            yield


def execute(name, command, env):
    if command[0] == 'cargo':
        slot = wait_for_slot(REPO / 'scripts/cargo-slots.sh', timeout=600, interval=15)
        print(slot.stdout + slot.stderr, end='', flush=True)
    print(f'--- {name} ---', flush=True)
    result = run_owned(command, cwd=REPO / 'src', env=env,
                       timeout=900 if '--no-run' in command else 240)
    print(result.stdout + result.stderr, end='', flush=True)
    print(f'{name}: exit={result.returncode}', flush=True)
    return result


def compile_mutant(name, env):
    result = execute(name + '-compile',
                     ['cargo', 'test', '-p', 'pilotdeck-gateway', '--test', 'recorder',
                      '--no-run'], env)
    if result.returncode != 0:
        raise RuntimeError('#1139 mutant did not compile; never a kill')


def generator_status(result, actual):
    lines = result.stderr.splitlines()
    if result.returncode == 0:
        return 'SURVIVED', 'generator accepted the wrong scenario count'
    if (result.returncode == 1
            and 'AssertionError [ERR_ASSERTION]: recorder oracle scenario count' in lines
            and f'{actual} !== 28' in lines):
        return 'KILLED', f'bound generator count assertion: {actual} !== 28'
    return 'INVALID', 'missing bound generator count assertion; never a kill'


def main():
    results = []
    # Allow the replay/generation suite plus cargo-slot and compilation headroom.
    # README's outer timeout leaves another 60 seconds for cleanup.
    with scratch('recorder-count', deadline=1800) as root, guarded_inputs():
        env = environment(root)
        baseline = os.environ['PDRS_BASELINE']
        oracle_path = root / 'oracle.json'
        original = (FIXTURES / 'oracle.json').read_bytes()
        env['PDRS_RECORDER_ORACLE'] = str(oracle_path)
        for name, change in [('drop', -1), ('add', 1)]:
            label = f'{name}-recorder-scenario'
            row = dict(mutation=label, test=TEST_NAME,
                       assertion='recorder oracle scenario count',
                       status='INVALID', reason='not run', compiled=False)
            try:
                oracle_path.write_bytes(original)
                if not passed(execute(label + '-positive', TEST, env), TEST_NAME):
                    raise RuntimeError('#1139 positive Rust replay failed')
                oracle = json.loads(original)
                assert len(oracle['scenarios']) == 28, '#1139 positive oracle scenario count'
                if change < 0:
                    oracle['scenarios'].pop()
                else:
                    oracle['scenarios'].append(copy.deepcopy(oracle['scenarios'][-1]))
                oracle_path.write_text(json.dumps(oracle) + '\n')
                compile_mutant(label, env)
                row['compiled'] = True
                case = Case(label, oracle_path, '', '', TEST_NAME, row['assertion'])
                row['status'], row['reason'] = classify(execute(label, TEST, env), case)
            except Exception as error:
                row.update(status='INVALID', reason=f'{type(error).__name__}: {error}')
            finally:
                oracle_path.write_bytes(original)
            try:
                if not passed(execute(label + '-restored', TEST, env), TEST_NAME):
                    raise RuntimeError('#1139 restored Rust replay failed')
            except Exception as error:
                row.update(status='INVALID', reason=row['reason'] + f'; restored: {error}')
            results.append(row)

        source = (FIXTURES / 'generate.mts').read_text()
        guard = "  assert.equal(scenarios.length, 28, 'recorder oracle scenario count');"
        for name, mutation, actual in [
            ('drop', '  scenarios.pop();', 27),
            ('add', '  scenarios.push(structuredClone(scenarios.at(-1)));', 29),
        ]:
            label = f'{name}-generated-recorder-scenario'
            row = dict(mutation=label, test='recorder/generate.mts::scenario-count',
                       assertion=f'recorder oracle scenario count: {actual} !== 28',
                       status='INVALID', reason='not run', compiled=False)
            try:
                # Mirroring from the repository root also keeps ../_shared and
                # ../../baseline_source_guard.mjs imports inside scratch (#1139).
                generator_path = copy_local_import_closure(
                    FIXTURES / 'generate.mts', REPO, root / name)
                assert source.count(guard) == 1, '#1139 unique generator count guard'
                generator_path.write_text(source.replace(guard, mutation + '\n' + guard))
                output = root / f'{name}-generated'
                command = ['node', '--import', f'{baseline}/node_modules/tsx/dist/loader.mjs',
                           str(generator_path), str(output)]
                compile_mutant(label, env)
                row['compiled'] = True
                row['status'], row['reason'] = generator_status(execute(label, command, env), actual)
                if row['status'] == 'KILLED':
                    assert not (output / 'oracle.json').exists(), '#1139 failed generator published an oracle'
                generator_path.write_text(source)
                command[-1] = str(root / f'{name}-restored')
                restored = execute(label + '-restored', command, env)
                if restored.returncode != 0:
                    raise RuntimeError('#1139 restored generator failed')
                restored_oracle = json.loads((Path(command[-1]) / 'oracle.json').read_text())
                assert len(restored_oracle['scenarios']) == 28, '#1139 restored generator scenario count'
            except Exception as error:
                row.update(status='INVALID', reason=f'{type(error).__name__}: {error}')
            results.append(row)
    for row in results:
        print(json.dumps(row), flush=True)
    counts = {status: sum(row['status'] == status for row in results)
              for status in ('KILLED', 'SURVIVED', 'INVALID')}
    print('SUMMARY: ' + ' '.join(f'{status}={count}' for status, count in counts.items()), flush=True)
    if len(results) != 4 or any(row['status'] != 'KILLED' for row in results):
        return 1
    print('PASS: 4 / 4 recorder count mutants killed', flush=True)
    return 0


if __name__ == '__main__':
    raise SystemExit(main())
