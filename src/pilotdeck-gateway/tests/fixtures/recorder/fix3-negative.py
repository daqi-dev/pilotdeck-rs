#!/usr/bin/env python3
# anchor-selfcheck: unsupported imperative replacement chain, no mutation table
"""#1139 recorder count guards reject missing and added TS scenarios.

#280 moved recorder/generate.mts onto the shared provenance guard, so the old
single-file copy died on ERR_MODULE_NOT_FOUND before the count assertion and
never judged the mutant. Like run-registry count-negative (#1129),
copy the generator's whole literal local import closure with the repository
layout; a missing closure file or one escaping the repository is an error.

The Rust criterion's compile inputs are guarded through the shared #1166
proc-macro2 scanner inside mutating_sources; this runner keeps no Rust lexer
of its own and only anchors the criterion entry in the scanner's input set.

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
from urllib.parse import unquote, urlparse

# Kept in sync with run-registry/count-negative.py by hand; extracting the
# shared closure/guard helpers is M6 #1099 (#1139 sibling review).

sys.dont_write_bytecode = True
sys.path.insert(0, str(Path(__file__).resolve().parents[1] / '_shared'))
from negative_control import environment, scratch
from negative_process import (_workspace_literal_inputs, mutating_sources,
                              run_owned, wait_for_slot)
from negative_suite import Case, classify, passed

FIXTURES = Path(__file__).resolve().parent
REPO = FIXTURES.parents[4]
TEST_NAME = 'real_ts_scenarios_match_outcomes_and_bytes'
TEST = ['cargo', 'test', '-p', 'pilotdeck-gateway', '--test', 'recorder',
        TEST_NAME, '--', '--exact', '--nocapture', '--test-threads=4']
# The Rust criterion entry whose count assertion binds every kill below; the
# scanner prefixes each literal input with this marker.
CRITERION_ENTRY = 'src/pilotdeck-gateway/tests/recorder.rs'
LITERAL_PREFIX = ':(top,literal)'


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


# The only allowed computed dynamic-import arguments, verbatim: the two fixed
# TS baseline loaders. recorder/generate.mts loads per-file sources with
# ('src', path); run-registry/generate.mts loads one full 'src/...' path. Both
# resolve inside PDRS_BASELINE, whose SHA-256 hashes oracle.json records, and
# whitespace runs collapse before the comparison. Every other shape — templates
# with substitutions, variables, concatenations, and arguments that merely
# mention baseline — is rejected, never silently skipped (#1139: a
# whitelist, not a substring search).
BASELINE_LOADERS = (
    "pathToFileURL(join(baseline, 'src', path)).href",
    'pathToFileURL(join(baseline, source)).href',
)


def _argument_without_trailing(argument, path):
    """Strip trailing block comments and commas from a dynamic import argument.

    Leading comments were already skipped by the argument scanner; a trailing
    comment or comma (``import('./a.mjs' /* x */)``, ``import('./a.mjs',)``)
    still denotes one literal specifier, so it is stripped before matching.
    """
    stripped = argument.rstrip()
    while stripped.endswith(',') or stripped.endswith('*/'):
        if stripped.endswith(','):
            stripped = stripped[:-1].rstrip()
            continue
        start = stripped.rfind('/*')
        if start < 0:
            raise ValueError(
                f'#1139 unterminated comment in dynamic import in {path}: {argument}')
        stripped = stripped[:start].rstrip()
    return stripped


def _dynamic_import_targets(path, argument, source_root):
    """Closure members of one dynamic import argument; unprovable is an error.

    Only two argument shapes are accepted: literal specifiers (quoted,
    comment-free templates) and the fixed baseline loaders listed above.
    Everything else — templates with substitutions, variables, concatenations
    and arguments that merely mention baseline — is an error, never a silently
    skipped dependency (#1139: a whitelist, never a substring search). file: URLs
    are local dependencies and follow the absolute-path rules; node: builtins
    skip like a static import; every other scheme is an error.
    """
    original = argument
    argument = _argument_without_trailing(argument, path)
    quoted = re.fullmatch(r'''(['"])([^'"\n]*)\1''', argument)
    if quoted is not None:
        specifier = quoted[2]
    elif re.fullmatch(r'`[^`]*`', argument) is not None and '${' not in argument:
        specifier = argument[1:-1]
    elif re.sub(r'\s+', ' ', argument) in BASELINE_LOADERS:
        return []
    else:
        raise ValueError(
            f'#1139 unrecognized dynamic import argument in {path}: {original}')
    if specifier.startswith(('./', '../')):
        return [path.parent / specifier]
    scheme = re.match(r'[A-Za-z][A-Za-z0-9+.\-]*:', specifier)
    if scheme is not None:
        if scheme[0] == 'node:':
            return []  # Node builtin module, never a repository file
        if scheme[0] != 'file:':
            raise ValueError(
                f'#1139 unsupported dynamic import scheme in {path}: {original}')
        parsed = urlparse(specifier)
        if (parsed.netloc not in ('', 'localhost')
                or parsed.query or parsed.fragment):
            raise ValueError(
                f'#1139 dynamic import file URL is not a plain local path '
                f'in {path}: {original}')
        specifier = unquote(parsed.path)
        if not specifier.startswith('/'):
            raise ValueError(
                f'#1139 dynamic import file URL lacks an absolute path in {path}: {original}')
    if not specifier.startswith('/'):
        return []  # bare external module specifier, like a static import
    resolved = Path(specifier).resolve()
    if resolved.is_relative_to(source_root):
        return [resolved]
    raise ValueError(
        f'#1139 absolute dynamic import outside the repository in {path}: {original}')


def _static_import_specifiers(text, path):
    """Literal string specifiers of static imports and re-exports in ``text``.

    Covers ``import './x'``, ``import x from './x'`` and ``export ... from
    './x'`` in any spacing, including zero whitespace and comments between the
    keyword and the string — forms real loaders accept and the old
    ``\\bimport\\s+`` regex silently dropped (#1139). Dynamic
    ``import(`` and ``import.meta`` never reach a string here.
    """
    specifiers = []
    for keyword in ('import', 'from'):
        for found in re.finditer(rf'\b{keyword}\b', text):
            index = _skip_spacing(text, found.end())
            if index >= len(text) or text[index] not in '\'"':
                continue
            quote, end = text[index], index + 1
            while end < len(text) and text[end] not in (quote, '\n'):
                end += 2 if text[end] == '\\' else 1
            if end >= len(text) or text[end] != quote:
                raise ValueError(
                    f'#1139 unterminated static import specifier in {path}')
            specifiers.append(text[index + 1:end])
    return specifiers


def local_import_closure(entry, source_root):
    """Find literal relative ESM imports, retaining their repository layout.

    Static from/import string specifiers (including re-exports and
    side-effect imports) are scanned spacing- and comment-aware; dynamic
    imports go through the fail-closed argument scanner above. This fixture
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
        for specifier in _static_import_specifiers(text, path):
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


def rust_criterion_inputs():
    """Rust build inputs the shared #1166 proc-macro2 scanner discovers.

    The scanner is the one Rust lexer here (#1174): mutating_sources already
    proves every workspace literal input equals HEAD through it, which covers
    the criterion chain (tests/recorder.rs and everything its mod/#[path]/
    include_*! declarations compile in, e.g. common/baseline.rs ->
    regeneration_masks.rs -> _shared/regeneration-masks.tsv). The entry anchor
    below fails closed when the criterion is no longer a scanner input, so a
    moved or renamed criterion is an error before any control runs, never an
    under-guarded KILLED.
    """
    inputs = {name.removeprefix(LITERAL_PREFIX)
              for name in _workspace_literal_inputs(REPO, 'HEAD')}
    if CRITERION_ENTRY not in inputs:
        raise RuntimeError(
            f'#1139 Rust criterion entry is not a #1166 scanner input: '
            f'{CRITERION_ENTRY}')
    return inputs


@contextmanager
def guarded_inputs():
    fixture_root = FIXTURES.parent
    generator = FIXTURES / 'generate.mts'
    # The Rust criterion compiles from tests/recorder.rs's own declarations;
    # mutating_sources proves every workspace literal input equals HEAD through
    # the shared #1166 proc-macro2 scanner, so a dirty criterion is refused
    # before any positive control. The entry anchor above keeps that coverage
    # pointed at this criterion; run-registry's bound test sits inside the
    # product tree the same shared check already covers.
    guarded = [generator, FIXTURES / 'oracle.json', Path(__file__).resolve(),
               fixture_root / '_shared/negative_control.py',
               *(fixture_root / name for name in (
                   'negative_process.py', 'negative_suite.py',
                   'negative_panic.py', 'pending_reanchor.py',
               ))]
    # Discover imports only after the entry and runner are checked under the
    # tree lock; keep that same reentrant lock through copying and all controls.
    with mutating_sources(REPO, guarded):
        rust_criterion_inputs()
        with mutating_sources(REPO, local_import_closure(generator, REPO)):
            yield


def execute(name, command, env):
    if command[0] == 'cargo':
        slot = wait_for_slot(REPO / 'scripts/cargo-slots.sh', timeout=600, interval=15)
        print(slot.stdout + slot.stderr, end='', flush=True)
    print(f'--- {name} ---', flush=True)
    # Every cargo call may pay a cold compile (the first positive control
    # included), so all cargo commands share the 900 s compilation window;
    # node runs keep the 240 s runtime window (#1139).
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
            # mutated generator itself, and compiled stays an explicit False
            # so the row schema matches the Rust rows (#1139).
            row = dict(mutation=label, test='recorder/generate.mts::scenario-count',
                       assertion=f'recorder oracle scenario count: {actual} !== 28',
                       status='INVALID', reason='not run', compiled=False,
                       receipt='not run')
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
