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

# Kept in sync with run-registry/count-negative.py by hand; extracting the
# shared closure/guard helpers is M6 #1099 (#1139 fix-r1 sibling review).

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


def _skip_spacing(text, index):
    """Skip whitespace and comments; an unterminated comment fails closed."""
    while index < len(text):
        if text[index].isspace():
            index += 1
        elif text.startswith('//', index):
            end = text.find('\n', index)
            index = len(text) if end < 0 else end + 1
        elif text.startswith('/*', index):
            end = text.find('*/', index + 2)
            if end < 0:
                raise ValueError(f'#1139 unterminated comment at offset {index}')
            index = end + 2
        else:
            break
    return index


def _dynamic_import_arguments(text):
    """Raw argument text of every dynamic ``import(...)`` call in ``text``.

    Comments and whitespace between ``import`` and ``(`` are allowed, matching
    real loaders; parentheses inside quoted strings or templates do not end the
    argument. Static declarations (``import x from`` / ``import 'x'``) and
    ``import.meta`` never reach the argument scan.
    """
    arguments = []
    for found in re.finditer(r'\bimport\b', text):
        index = _skip_spacing(text, found.end())
        if index >= len(text) or text[index] != '(':
            continue
        start = _skip_spacing(text, index + 1)
        index, depth, quote = start, 1, ''
        while index < len(text) and depth:
            character = text[index]
            if quote:
                if character == '\\':
                    index += 1
                elif character == quote:
                    quote = ''
            elif character in '\'"`':
                quote = character
            elif character == '(':
                depth += 1
            elif character == ')':
                depth -= 1
            index += 1
        if depth:
            raise ValueError(f'#1139 unterminated dynamic import at offset {start}')
        arguments.append(text[start:index - 1].strip())
    return arguments


def _dynamic_import_targets(path, argument, source_root):
    """Closure members of one dynamic import argument; unprovable is an error.

    Literal specifiers (quoted, comment-free templates) join the closure when
    relative or inside the repository. Everything the closure cannot prove —
    templates with substitutions, variables, concatenations and absolute paths
    outside the repository — is an error, never a silently skipped dependency
    (#1139 fix-r1 P1). The one allowed computed specifier is the fixed TS
    baseline loader: those sources live outside the repository and oracle.json
    records their SHA-256 hashes.
    """
    quoted = re.fullmatch(r'''(['"])([^'"\n]*)\1''', argument)
    if quoted is not None:
        specifier = quoted[2]
    elif re.fullmatch(r'`[^`]*`', argument) is not None and '${' not in argument:
        specifier = argument[1:-1]
    else:
        if re.search(r'\bbaseline\b', argument):
            return []
        raise ValueError(
            f'#1139 unrecognized dynamic import argument in {path}: {argument}')
    if specifier.startswith(('./', '../')):
        return [path.parent / specifier]
    if not specifier.startswith('/'):
        return []  # bare external module specifier, like a static import
    resolved = Path(specifier).resolve()
    if resolved.is_relative_to(source_root):
        return [resolved]
    raise ValueError(
        f'#1139 absolute dynamic import outside the repository in {path}: {argument}')


def local_import_closure(entry, source_root):
    """Find literal relative ESM imports, retaining their repository layout.

    Like baseline_source_guard.mjs, scan from/import string specifiers
    (including re-exports and side-effect imports). Dynamic imports go through
    the fail-closed argument scanner above, so a comment-separated or template
    literal still joins the closure while anything unprovable is an error.
    This fixture closure uses exact filenames; missing files are errors, never
    skipped.
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
        for match in re.finditer(r'''(?:\bfrom\s*|\bimport\s+)(['"])([^'"]+)\1''', text):
            specifier = match[2]
            if specifier.startswith(('./', '../')):
                pending.append(path.parent / specifier)
        for argument in _dynamic_import_arguments(text):
            pending.extend(_dynamic_import_targets(path, argument, source_root))
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
    # The Rust criterion must equal HEAD too, not only the generator side:
    # tests/recorder.rs binds #[path] common/baseline.rs -> regeneration_masks.rs,
    # whose include_str! compiles in _shared/regeneration-masks.tsv, and the
    # crate manifest selects the build; a dirty criterion could publish KILLED
    # from unreviewed bytes (#1139 fix-r1 P1). run-registry's bound test sits
    # inside the product tree its shared pristine check already covers.
    gateway = REPO / 'src/pilotdeck-gateway'
    guarded = [generator, FIXTURES / 'oracle.json', Path(__file__).resolve(),
               fixture_root / '_shared/negative_control.py',
               *(fixture_root / name for name in (
                   'negative_process.py', 'negative_suite.py',
                   'negative_panic.py', 'pending_reanchor.py',
               )),
               gateway / 'tests/recorder.rs',
               gateway / 'tests/common/baseline.rs',
               gateway / 'tests/common/regeneration_masks.rs',
               fixture_root / '_shared/regeneration-masks.tsv',
               gateway / 'Cargo.toml']
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
    # Every cargo call may pay a cold compile (the first positive control
    # included), so all cargo commands share the 900 s compilation window;
    # node runs keep the 240 s runtime window (#1139 fix-r1 P2).
    result = run_owned(command, cwd=REPO / 'src', env=env,
                       timeout=900 if command[0] == 'cargo' else 240)
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
            # cargo would compile unmutated Rust here, so generator mutants
            # carry no cargo receipt: their receipt is the node run of the
            # mutated generator itself (#1139 fix-r1 P2).
            row = dict(mutation=label, test='recorder/generate.mts::scenario-count',
                       assertion=f'recorder oracle scenario count: {actual} !== 28',
                       status='INVALID', reason='not run', receipt='not run')
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
                result = execute(label, command, env)
                row['receipt'] = f'generator mutant run: node exit={result.returncode}'
                row['status'], row['reason'] = generator_status(result, actual)
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
