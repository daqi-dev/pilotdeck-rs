#!/usr/bin/env python3
# anchor-selfcheck: unsupported disposable Python meta-controls with verified import bootstrap
"""#994 shared runner meta-controls; commit first, then run against real HEAD.

No cargo is launched: each Python mutant is syntax-checked before its bound
unittest runs. Every control restores, reruns, and contributes to the summary.
"""
import ast
import builtins
import importlib.abc
import importlib.util
import json
import os
from contextlib import contextmanager
from contextvars import ContextVar
from pathlib import Path
import subprocess
import sys
import tempfile

root = Path(__file__).resolve().parents[5]
fixtures = root / 'src/pilotdeck-gateway/tests/fixtures'
shared = fixtures / '_shared/negative_control.py'
process = fixtures / 'negative_process.py'
tests = fixtures / 'test_negative_adaptive.py'
_process_api = ContextVar('verified_process_api')
cases = [
    ('floor-too-small', shared, 'MIN_RUN_SECONDS = 20', 'MIN_RUN_SECONDS = 1',
     'AdaptiveTests.test_floor_and_multiplier', '#994 minimum runtime'),
    ('multiplier-too-small', shared, 'RUN_FACTOR = 5', 'RUN_FACTOR = 1',
     'AdaptiveTests.test_floor_and_multiplier', '#994 runtime limit is baseline times five'),
    ('compile-wall-clock', shared, "command, timeout=None,\n                                 idle_timeout=COMPILE_IDLE_SECONDS",
     "command, timeout=20,\n                                 idle_timeout=COMPILE_IDLE_SECONDS",
     'CargoTests.test_compile_is_separate_and_runtime_uses_baseline', '#994 compile must not use test wall clock'),
    ('compile-runs-tests', shared, "'--no-run', '--message-format=json'", "'--message-format=json'",
     'CargoTests.test_compile_is_separate_and_runtime_uses_baseline', '#994 must compile without running tests'),
    ('runtime-fixed-budget', shared, 'else adaptive_timeout(self.baselines[key]))', 'else BASELINE_RUN_SECONDS)',
     'CargoTests.test_compile_is_separate_and_runtime_uses_baseline', '#994 exclude compile duration'),
    ('compile-error-accepted', shared, 'if compiled.returncode:', 'if False and compiled.returncode:',
     'CargoTests.test_compile_failure_never_runs_tests', '#994 compile failure is invalid'),
    ('skip-restored-rebuild', shared, "        slot = wait_for_slot(REPO / 'scripts/cargo-slots.sh', timeout=900)",
     "        if restored:\n            return subprocess.CompletedProcess([], 0, 'test result: ok.', '')\n        slot = wait_for_slot(REPO / 'scripts/cargo-slots.sh', timeout=900)",
     'CargoTests.test_restored_rebuilds_in_the_same_target_and_does_not_recalibrate', '#994 restored must rebuild'),
    ('stop-after-first', shared, 'for name, path, old, new, tests in rows:', 'for name, path, old, new, tests in rows[:1]:',
     'SuiteTests.test_all_mutations_restore_and_pass', '#994 complete accounting'),
    ('skip-scratch-restore', shared, 'copied.write_bytes(original)', 'copied.write_bytes(b"changed")',
     'SuiteTests.test_all_mutations_restore_and_pass', '#994 scratch restored'),
    ('allow-product-target', shared, 'if any(target == path.resolve() or path.resolve() in target.parents',
     'if False and any(target == path.resolve() or path.resolve() in target.parents',
     'AdaptiveTests.test_target_is_caller_owned_and_separate', '#994 reject product/disposable target'),
    ('nonfinite-idle-accepted', process,
     'if idle_timeout is not None and (not math.isfinite(idle_timeout) or idle_timeout <= 0):',
     'if False and idle_timeout is not None and (not math.isfinite(idle_timeout) or idle_timeout <= 0):',
     'ProgressTests.test_invalid_idle_deadline_fails_before_spawn', '#994 invalid idle deadline must not spawn'),
]


class SourceBaselineError(RuntimeError):
    pass


def git_bytes(*args):
    # Independent bootstrap: never ask the module being checked to verify itself.
    return subprocess.check_output(['git', '-C', str(root), *args], timeout=30)


def input_paths(commit):
    entries = git_bytes('ls-tree', '-r', '-z', '--name-only', commit, '--',
                        fixtures.relative_to(root).as_posix())
    available = {}
    for entry in entries.split(b'\0'):
        if not entry:
            continue
        path = root / os.fsdecode(entry)
        relative = path.relative_to(fixtures)
        if path.suffix == '.py' and (relative.parts[0] == '_shared' or
                (path.parent == fixtures and not path.name.startswith('test_')) or path == tests):
            parts = list(relative.with_suffix('').parts)
            if parts[-1] == '__init__':
                parts.pop()
            available['.'.join(parts)] = path
    pending = ['_shared.negative_control', 'negative_process', 'test_negative_adaptive']
    if any(name not in available for name in pending):
        raise SourceBaselineError('#994 required inputs missing from HEAD')
    selected = {}
    while pending:
        name = pending.pop()
        if name in selected:
            continue
        path = selected[name] = available[name]
        package = name if path.name == '__init__.py' else name.rpartition('.')[0]
        source = git_bytes('show', f'{commit}:{path.relative_to(root).as_posix()}')
        imports = []
        for node in ast.walk(ast.parse(source)):
            if isinstance(node, ast.Import):
                imports.extend(alias.name for alias in node.names)
            elif isinstance(node, ast.ImportFrom):
                module = node.module or ''
                if node.level:
                    module = importlib.util.resolve_name('.' * node.level + module, package)
                imports.append(module)
                imports.extend(f'{module}.{alias.name}' for alias in node.names)
        # Include package initializers as well as transitive, function-local and
        # relative imports. Sources and the inventory come from one fixed OID.
        for imported in [name, *imports]:
            parts = imported.split('.')
            for index in range(1, len(parts) + 1):
                dependency = '.'.join(parts[:index])
                if dependency in available and dependency not in selected:
                    pending.append(dependency)
    return sorted(selected.values())


def verify_bytes(contents, message):
    for path, data in contents.items():
        if not path.is_file() or path.read_bytes() != data:
            raise SourceBaselineError(f'#994 {path.relative_to(root)}: {message}')


@contextmanager
def trusted_process(contents):
    """Load local imports only from independently verified, committed bytes.

    A private import scope also ignores a stale negative_process in sys.modules.
    No copied Git/build state, and no working-tree module executes at bootstrap.
    """
    modules = {}
    packages = {'_shared'}
    for path, data in contents.items():
        parts = list(path.relative_to(fixtures).with_suffix('').parts)
        if parts[-1] == '__init__':
            parts.pop()
            packages.add('.'.join(parts))
        for index in range(1, len(parts)):
            packages.add('.'.join(parts[:index]))
        modules['.'.join(parts)] = (path, data)
    names = set(modules) | packages

    def verified_import(name, globals=None, locals=None, fromlist=(), level=0):
        absolute = (importlib.util.resolve_name('.' * level + name, globals['__package__'])
                    if level else name)
        if absolute not in names and absolute.split('.')[0] not in sys.stdlib_module_names:
            raise ImportError(f'#994 unverified shared import: {absolute}')
        return builtins.__import__(name, globals, locals, fromlist, level)

    class VerifiedLoader(importlib.abc.MetaPathFinder, importlib.abc.Loader):
        def find_spec(self, fullname, path=None, target=None):
            if fullname in names:
                return importlib.util.spec_from_loader(fullname, self, is_package=fullname in packages)
            if fullname.split('.')[0] not in sys.stdlib_module_names:
                raise ImportError(f'#994 unverified shared import: {fullname}')
            return None

        def create_module(self, spec):
            return None

        def exec_module(self, module):
            if module.__name__ in modules:
                path, data = modules[module.__name__]
                module.__file__ = str(path)
                module.__dict__['__builtins__'] = dict(vars(builtins), __import__=verified_import)
                exec(compile(data, str(path), 'exec'), module.__dict__)

    roots = {name.split('.')[0] for name in names}
    saved = {name: sys.modules.pop(name) for name in list(sys.modules) if name.split('.')[0] in roots}
    search_path = sys.path[:]
    loader = VerifiedLoader()
    sys.meta_path.insert(0, loader)
    try:
        yield importlib.import_module('negative_process')
    finally:
        sys.meta_path.remove(loader)
        for name in list(sys.modules):
            if name.split('.')[0] in roots:
                sys.modules.pop(name, None)
        sys.modules.update(saved)
        sys.path[:] = search_path


@contextmanager
def verified_inputs():
    commit = git_bytes('rev-parse', 'HEAD').decode().strip()
    contents = {path: git_bytes('show', f'{commit}:{path.relative_to(root).as_posix()}')
                for path in input_paths(commit)}
    verify_bytes(contents, 'differs from HEAD')
    with trusted_process(contents) as api, api.mutating_sources(root, [], baseline=commit):
        verify_bytes(contents, 'differs from HEAD')
        token = _process_api.set(api)
        try:
            yield contents
        finally:
            _process_api.reset(token)
            verify_bytes(contents, 'changed during isolated controls')
            if git_bytes('rev-parse', 'HEAD').decode().strip() != commit:
                raise SourceBaselineError('#994 HEAD changed during isolated controls')


@contextmanager
def isolated_inputs(base, contents, out):
    """Copy verified Python inputs only; never create Git/build state.

    Keeping the repository-relative layout also makes __file__-based imports
    select the copied shared modules. SIGKILL may leave this disposable copy,
    but cannot leave a mutant in the caller's checkout.
    """
    with tempfile.TemporaryDirectory(prefix='pdrs994-meta-', dir=out) as name:
        directory = Path(name)
        for path, data in contents.items():
            copied = directory / path.relative_to(base)
            copied.parent.mkdir(parents=True, exist_ok=True)
            copied.write_bytes(data)
        yield directory


def run_case(case, directory, out, original):
    name, path, before, after, test, marker = case
    copied = directory / path.relative_to(root)
    command = [sys.executable, '-B', str(directory / tests.relative_to(root)), test]
    row = dict(name=name, status='INVALID', reason='not run', compiled=False, restoredPass=False)

    def run(label, argv):
        # Fresh bytecode namespace also keeps py_compile output off the real
        # tree. -B prevents stale same-second imports of equal-sized mutants.
        env = dict(os.environ, PYTHONPYCACHEPREFIX=str(out / (label + '-cache')))
        result = _process_api.get().run_owned(argv, cwd=directory, env=env, timeout=30)
        (out / (label + '.log')).write_text(result.stdout + result.stderr)
        return result

    try:
        text = original.decode()
        if text.count(before) != 1:
            raise ValueError(f'stale anchor: {name}')
        result = run(name + '-positive', command)
        if result.returncode != 0:
            raise RuntimeError('bound positive unittest did not pass')
        copied.write_bytes(text.replace(before, after, 1).encode())
        compiled = run(name + '-compile', [sys.executable, '-m', 'py_compile', str(copied)])
        if compiled.returncode != 0:
            raise RuntimeError('Python mutant does not compile; never a kill')
        row['compiled'] = True
        result = run(name + '-negative', command)
        log = result.stdout + result.stderr
        failed = f'FAIL: {test.split(".")[-1]} ('
        block = log.partition(failed)[2].split('\n====')[0]
        if result.returncode and block and marker in block:
            row.update(status='KILLED', reason='bound #994 unittest assertion failed')
        elif result.returncode == 0:
            row.update(status='SURVIVED', reason='bound unittest still passed')
        else:
            row.update(reason='unrelated error or missing named assertion')
    except Exception as error:
        row.update(status='INVALID', reason=f'{type(error).__name__}: {error}')
    finally:
        # A failed/no-op restoration must never leave a KILLED record behind.
        # Only the disposable path is written, including on exceptional exit.
        copied.write_bytes(original)
    if copied.read_bytes() != original:
        row.update(status='INVALID', reason=row['reason'] + '; scratch restoration failed')
    else:
        row['restoredPass'] = run(name + '-restored', command).returncode == 0
        if not row['restoredPass']:
            row.update(status='INVALID', reason=row['reason'] + '; restored unittest failed')
    return row


def main():
    if not os.environ.get('CARGO_TARGET_DIR'):
        raise SystemExit('CARGO_TARGET_DIR must be supplied by the caller')
    if len(sys.argv) != 2:
        raise SystemExit('usage: negative_controls.py NEW_OUTPUT_DIR')
    out = Path(sys.argv[1]).resolve()
    out.mkdir(parents=True, exist_ok=False)
    records = []
    for case in cases:
        name, path, before, after, test, marker = case
        row = dict(name=name, status='INVALID', reason='not run', compiled=False, restoredPass=False)
        try:
            # Verify BOTH executors before importing either into a child.
            # The real tree is locked, but all mutation writes target copies.
            with verified_inputs() as contents:
                with isolated_inputs(root, contents, out) as directory:
                    row = run_case(case, directory, out, contents[path])
        except Exception as error:
            row.update(status='INVALID', reason=f'{type(error).__name__}: {error}', restoredPass=False)
        records.append(row)
        print(f'{name}: {row["status"]}: {row["reason"]}', flush=True)
    counts = {state: sum(row['status'] == state for row in records)
              for state in ('KILLED', 'SURVIVED', 'INVALID')}
    (out / 'summary.json').write_text(json.dumps({'controls': records, 'counts': counts}, indent=2) + '\n')
    print('summary: ' + ', '.join(f'{state}={count}' for state, count in counts.items()), flush=True)
    return int(not records or len(records) != len(cases) or
               any(row['status'] != 'KILLED' for row in records))


if __name__ == '__main__':
    raise SystemExit(main())
