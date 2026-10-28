#!/usr/bin/env python3
# anchor-selfcheck: unsupported environment imposters and in-memory AST source checks, no in-place source replacement table
"""#701 item 3 negative controls: cli-parity entries must ignore same-dir imposters.

Environment rows place imposter packages/extensions in a private mirror;
source rows feed compiled mutants to named unittest assertions in memory or
in the unit test's temporary Python mirror, never in checkout source files.
KILLED requires the bound assertion or an environment witness proving the
imposter cannot execute. SURVIVED means the old behaviour returned; INVALID
means the test could not run, compile, or reach the exclusive assertion.

No environment row writes to the checkout. Every mirror is removed on exit;
run all rows and summarise; any non-KILLED row is a nonzero exit.
"""
import sys

if not sys.flags.isolated or __file__.rpartition('/')[0] in sys.path:
    raise SystemExit(f'#701: unsafe import path; run with -I by file: python3 -I {__file__}')

import argparse
import ast
import importlib.util
import json
import io
import os
from pathlib import Path
import shutil
import subprocess
import sysconfig
import tempfile
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[5]
DEFAULT_APPARATUS = ROOT / 'scripts/cli-parity'
IMPOSTERS = (('common', 'compare.py'), ('policy', 'compare.py'),
             ('compare', 'controls.py'))
MARKER_ENV = 'PDRS_CLI_PARITY_IMPOSTER'
EXTENSION_SOURCE = '''\
#include <Python.h>
#include <stdio.h>
#include <stdlib.h>

static struct PyModuleDef module = {PyModuleDef_HEAD_INIT, "common", NULL, -1, NULL};

PyMODINIT_FUNC PyInit_common(void) {
    const char *marker = getenv("PDRS_CLI_PARITY_IMPOSTER");
    if (marker) {
        FILE *stream = fopen(marker, "a");
        if (stream) {
            fputs("common-extension\\n", stream);
            fclose(stream);
        }
    }
    return PyModule_Create(&module);
}
'''


# Use the same source-only loader as the entries; this fixture has no copy.
_bootstrap_path = DEFAULT_APPARATUS / '_bootstrap.py'
_bootstrap_spec = importlib.util.spec_from_file_location('_cli_parity_bootstrap', _bootstrap_path)
_bootstrap = importlib.util.module_from_spec(_bootstrap_spec)
exec(compile(_bootstrap_path.read_bytes(), str(_bootstrap_path), 'exec'), _bootstrap.__dict__)
load = _bootstrap.load
GUARDED_FILES = _bootstrap.ENTRY_FILES
APPARATUS_FILES = _bootstrap.APPARATUS_FILES
APPARATUS_MODULES = _bootstrap.APPARATUS_MODULES
MIRROR_FILES = _bootstrap.MIRROR_FILES


def statements(tree):
    body = tree.body
    if (body and isinstance(body[0], ast.Expr) and isinstance(body[0].value, ast.Constant)
            and isinstance(body[0].value.value, str)):
        body = body[1:]
    return body


def guard_problem(tree):
    body = statements(tree)
    if len(body) < 2:
        return 'missing import sys and the isolated guard'
    first = body[0]
    if not (isinstance(first, ast.Import)
            and [alias.name for alias in first.names] == ['sys']):
        return f'first executable statement is {ast.unparse(first)[:80]!r}, not "import sys"'
    guard = body[1]
    if not isinstance(guard, ast.If):
        return 'second statement is not the isolated guard'
    expected = "not sys.flags.isolated or __file__.rpartition('/')[0] in sys.path"
    if ast.unparse(guard.test) != expected:
        return f'guard tests {ast.unparse(guard.test)!r}, not {expected!r}'
    if not any(isinstance(node, ast.Raise) for node in guard.body):
        return 'guard does not raise SystemExit'
    return None


def path_mutations(tree):
    found = []
    for node in ast.walk(tree):
        if (isinstance(node, ast.Call) and isinstance(node.func, ast.Attribute)
                and node.func.attr in ('insert', 'append', 'extend')):
            target = node.func.value
            if (isinstance(target, ast.Attribute) and target.attr == 'path'
                    and isinstance(target.value, ast.Name) and target.value.id == 'sys'):
                found.append(f'sys.path.{node.func.attr}')
    return found


def bare_imports(tree):
    found = []
    for node in ast.walk(tree):
        if isinstance(node, ast.ImportFrom) and node.module in APPARATUS_MODULES:
            found.append(f'from {node.module} import')
        elif isinstance(node, ast.Import):
            for alias in node.names:
                if alias.name.split('.')[0] in APPARATUS_MODULES:
                    found.append(f'import {alias.name}')
    return found


def imposter_source(kind):
    return ('import os\n'
            f'with open(os.environ[{MARKER_ENV!r}], "a") as stream:\n'
            f'    stream.write({kind!r} + "\\n")\n')


def run(command, marker, timeout=120):
    env = {key: value for key, value in os.environ.items()
           if key not in ('PYTHONPATH', 'PYTHONHOME', 'PYTHONSTARTUP')}
    env[MARKER_ENV] = str(marker)
    try:
        return subprocess.run(command, cwd=str(ROOT), env=env, capture_output=True,
                              text=True, timeout=timeout)
    except (OSError, subprocess.TimeoutExpired) as error:
        # Keep later rows runnable; a timeout/crash never supplies a kill.
        return subprocess.CompletedProcess(command, 124, '', f'{type(error).__name__}: {error}')


def record(rows, name, status, reason):
    rows.append({'name': name, 'status': status, 'reason': reason})
    print(f'{name}: {status}: {reason}', flush=True)


def static_checks(apparatus, rows):
    for name in APPARATUS_FILES:
        path = apparatus / name
        try:
            tree = ast.parse(path.read_text(encoding='utf-8'))
        except (OSError, SyntaxError) as error:
            record(rows, f'static/{name}', 'INVALID', f'unreadable source: {error}')
            continue
        if name in GUARDED_FILES:
            problem = guard_problem(tree)
            record(rows, f'guard-first/{name}', 'KILLED' if problem is None else 'SURVIVED',
                   problem or 'import sys + isolated guard are the first statements after the docstring')
        moved = path_mutations(tree)
        record(rows, f'no-path-mutation/{name}', 'KILLED' if not moved else 'SURVIVED',
               'no sys.path mutation' if not moved else ', '.join(moved))
        imports = bare_imports(tree)
        record(rows, f'no-bare-import/{name}', 'KILLED' if not imports else 'SURVIVED',
               'apparatus modules load by explicit path' if not imports
               else ', '.join(imports))


def no_isolated_checks(apparatus, rows, holder):
    imposter = apparatus / 'json.py'
    try:
        imposter.write_text(imposter_source('json.py'))
        for name in GUARDED_FILES:
            marker = holder / f'no-isolated-{name}.marker'
            marker.unlink(missing_ok=True)
            result = run([sys.executable, str(apparatus / name), '-h'], marker)
            message = result.stderr.strip()
            if marker.exists():
                record(rows, f'guard-before-stdlib/{name}', 'SURVIVED',
                       'json.py executed before the isolated guard')
            elif result.returncode == 1 and 'run with -I' in message:
                record(rows, f'guard-before-stdlib/{name}', 'KILLED',
                       f'exit 1 at the guard; imposter not executed ({message!r})')
            else:
                record(rows, f'guard-before-stdlib/{name}', 'INVALID',
                       f'rc={result.returncode}; stderr={message[:180]!r}')
    finally:
        imposter.unlink(missing_ok=True)


def package_checks(apparatus, rows, holder):
    for module, entry in IMPOSTERS:
        target = apparatus / module
        marker = holder / f'{module}-package.marker'
        witness_marker = holder / f'{module}-witness.marker'
        witness = holder / f'witness-{module}.py'
        if target.exists():
            record(rows, f'imposter/{module}-package', 'INVALID',
                   f'stale path already exists: {target}')
            continue
        try:
            target.mkdir()
            (target / '__init__.py').write_text(imposter_source(f'{module}-package'))
            compiled = subprocess.run(
                [sys.executable, '-I', '-m', 'py_compile', str(target / '__init__.py')],
                capture_output=True, text=True, timeout=60)
            if compiled.returncode:
                record(rows, f'imposter/{module}-package', 'INVALID',
                       f'imposter does not compile: {compiled.stderr.strip()[:160]}')
                continue
            marker.unlink(missing_ok=True)
            result = run([sys.executable, '-I', str(apparatus / entry), '-h'], marker)
            witness.write_text('import sys\n'
                               f'sys.path.insert(0, {str(apparatus)!r})\n'
                               f'import {module}\n')
            witness_marker.unlink(missing_ok=True)
            run([sys.executable, '-I', str(witness)], witness_marker)
            message = result.stderr.strip()
            if not witness_marker.exists():
                record(rows, f'imposter/{module}-package', 'INVALID',
                       'witness bare import did not load the imposter; resolver precedence not exercised')
            elif marker.exists():
                record(rows, f'imposter/{module}-package', 'SURVIVED',
                       f'{entry} executed the {module}/ imposter')
            elif result.returncode == 0:
                record(rows, f'imposter/{module}-package', 'KILLED',
                       f'{entry} loaded {module}.py by explicit path (rc=0); imposter not executed')
            else:
                record(rows, f'imposter/{module}-package', 'INVALID',
                       f'rc={result.returncode}; stderr={message[:180]!r}')
        except Exception as error:
            record(rows, f'imposter/{module}-package', 'INVALID', f'{type(error).__name__}: {error}')
        finally:
            shutil.rmtree(target, ignore_errors=True)


def extension_check(apparatus, rows, holder):
    """A CPython extension module wins over common.py under path-based import."""
    compiler = shutil.which('clang') or shutil.which('cc')
    suffix = sysconfig.get_config_var('EXT_SUFFIX')
    include = Path(sysconfig.get_paths()['include'])
    if not compiler or not suffix or not (include / 'Python.h').is_file():
        record(rows, 'imposter/common-extension', 'INVALID',
               'no C compiler or Python headers; extension substitution not exercised')
        return
    target = apparatus / f'common{suffix}'
    marker = holder / 'common-extension.marker'
    witness_marker = holder / 'common-extension-witness.marker'
    witness = holder / 'witness-common-extension.py'
    source = holder / 'imposter-common.c'
    flags = ['-shared', '-fPIC']
    if sys.platform == 'darwin':
        flags += ['-undefined', 'dynamic_lookup']
    try:
        source.write_text(EXTENSION_SOURCE)
        compiled = subprocess.run([compiler, *flags, f'-I{include}', '-o', str(target), str(source)],
                                  capture_output=True, text=True, timeout=120)
        if compiled.returncode:
            record(rows, 'imposter/common-extension', 'INVALID',
                   f'imposter extension does not compile: {compiled.stderr.strip()[:180]}')
            return
        marker.unlink(missing_ok=True)
        result = run([sys.executable, '-I', str(apparatus / 'compare.py'), '-h'], marker)
        witness.write_text('import sys\n'
                           f'sys.path.insert(0, {str(apparatus)!r})\n'
                           'import common\n')
        witness_marker.unlink(missing_ok=True)
        run([sys.executable, '-I', str(witness)], witness_marker)
        if not witness_marker.exists():
            record(rows, 'imposter/common-extension', 'INVALID',
                   'witness bare import did not load the extension; resolver precedence not exercised')
        elif marker.exists():
            record(rows, 'imposter/common-extension', 'SURVIVED',
                   'compare.py executed the common extension imposter')
        elif result.returncode == 0:
            record(rows, 'imposter/common-extension', 'KILLED',
                   'compare.py loaded common.py by explicit path (rc=0); extension not executed')
        else:
            record(rows, 'imposter/common-extension', 'INVALID',
                   f'rc={result.returncode}; stderr={result.stderr.strip()[:180]!r}')
    finally:
        target.unlink(missing_ok=True)


class AssertionResult(unittest.TextTestResult):
    """Retain actual exception identities; source/log echoes cannot earn a kill."""

    def __init__(self, *args, **kwargs):
        super().__init__(*args, **kwargs)
        self.assertions = []

    def addFailure(self, test, error):
        self.assertions.append((test.id(), error[0], str(error[1])))
        super().addFailure(test, error)

    def addSubTest(self, test, subtest, error):
        if error is not None:
            self.assertions.append((test.id(), error[0], str(error[1])))
        super().addSubTest(test, subtest, error)


def passed(result):
    return (result.testsRun == 1 and result.wasSuccessful() and not result.skipped)


def classify(result, test, assertion):
    if result.testsRun != 1 or result.errors or result.skipped:
        return 'INVALID', 'wrong test count, error, or skip'
    if result.wasSuccessful():
        return 'SURVIVED', 'bound test still passed'
    if (result.assertions
            and all(full_name == test and kind is AssertionError
                    for full_name, kind, _message in result.assertions)
            and any(assertion in message for _full_name, _kind, message in result.assertions)):
        return 'KILLED', 'bound test raised its exclusive assertion'
    return 'INVALID', 'wrong failure point or assertion'


def bootstrap_regressions(apparatus):
    """Compile source mutants before testing AST inputs or a temporary test mirror."""
    method = 'test_entries_use_shared_bootstrap'
    for entry in GUARDED_FILES:
        path = apparatus / entry
        original = path.read_text()
        yield (f'bootstrap/path-guard/{entry}', path,
               original.replace(" or __file__.rpartition('/')[0] in sys.path", '', 1),
               'test_isolated_guard_is_the_first_statement',
               f'#701: {entry}: the guard must reject non-isolated or reinserted paths')
        yield (f'bootstrap/local-load/{entry}', path,
               original + '\ndef _load(name, path):\n    return __import__(name)\n', method,
               f'#701: {entry}: entry must not define its own _load')
        yield (f'bootstrap/bare-load/{entry}', path,
               original.replace('_load = _bootstrap.load', '_load = __import__', 1), method,
               f'#701: {entry}: must load the shared bootstrap by explicit source path')
        for binding in ('_load', '_bootstrap', '_bootstrap_path', '_bootstrap_spec'):
            yield (f'bootstrap/rebind-{binding}/{entry}', path,
                   original + f'\n{binding} = None\n', method,
                   f'#701: {entry}: shared bootstrap binding {binding} must not change')
        yield (f'bootstrap/dynamic-import/{entry}', path,
               original + "\n__import__('common')\n", method,
               f'#701: {entry}: dynamic bare import is forbidden')
        yield (f'bootstrap/runtime-bare-load/{entry}', path,
               original.replace('_load = _bootstrap.load',
                                'def _load(name, path):\n    return __import__(name)', 1),
               'test_each_entry_starts_isolated_with_imposter_packages',
               f'#701: {entry}: isolated startup must ignore packages')
    path = apparatus / '_bootstrap.py'
    line = len(path.read_text().splitlines()) + 2
    yield ('bootstrap/library-bare-import', path, path.read_text() + '\nimport common\n',
           'test_apparatus_modules_load_by_explicit_path_only',
           f'#701: _bootstrap.py:{line}: bare apparatus import')
    yield ('bootstrap/library-path-mutation', path, path.read_text() + "\nsys.path.append('.')\n",
           'test_apparatus_modules_load_by_explicit_path_only',
           f'#701: _bootstrap.py:{line}: sys.path.append is forbidden')
    path = ROOT / 'src/pilotdeck-cli/tests/fixtures/chat-search/README.md'
    yield ('commands/chat-search-without-I', path,
           path.read_text().replace('python3 -I scripts/cli-parity/', 'python3 scripts/cli-parity/'),
           'test_documented_commands_use_isolated_mode',
           '#701: src/pilotdeck-cli/tests/fixtures/chat-search/README.md:16: CLI parity command requires -I')
    path = apparatus / 'test_cli_parity.py'
    yield ('discover/path-guard', path,
           path.read_text().replace(" or __file__.rpartition('/')[0] in sys.path", '', 1),
           'test_discover_rejects_reinserted_path_before_stdlib',
           '#701: discover must reject the reinserted path before importing json')
    path = apparatus / 'README.md'
    source = path.read_text()
    command = 'python3 -I scripts/cli-parity/run_round.py --help'
    for name, replacement in (
            ('versioned', 'python3.13 scripts/cli-parity/run_round.py --help'),
            ('quoted-path', 'python3 "$PWD/scripts/cli-parity/run_round.py" --help'),
            ('single-quoted-path', "python3 'scripts/cli-parity/run_round.py' --help"),
            ('variable-python', '$PY scripts/cli-parity/run_round.py --help'),
            ('quoted-python', '"python3.13" scripts/cli-parity/run_round.py --help'),
            ('quoted-variable-python', '"$PY" scripts/cli-parity/run_round.py --help'),
            ('warning-argument', 'python3 -W -I scripts/cli-parity/run_round.py --help'),
            ('xoption-argument', 'python3 -X -I scripts/cli-parity/run_round.py --help'),
            ('continued', 'python3 \\\n    scripts/cli-parity/run_round.py --help')):
        yield (f'commands/{name}', path, source.replace(command, replacement, 1),
               'test_documented_commands_use_isolated_mode',
               '#701: scripts/cli-parity/README.md:6: CLI parity command requires -I')
    yield ('commands/discover', path,
           source.replace('python3 -I scripts/cli-parity/test_cli_parity.py',
                          'python3 -I -m unittest discover -s scripts/cli-parity', 1),
           'test_documented_commands_use_isolated_mode',
           '#701: scripts/cli-parity/README.md:9: CLI parity command requires an explicit file')
    yield ('commands/missing-readme', path,
           source.replace('scripts/cli-parity', 'scripts/other-parity'),
           'test_documented_commands_use_isolated_mode',
           '#701: documented command inventory must include scripts/cli-parity/README.md')


def run_bound_test(module, method, path=None, source=None):
    read_text = Path.read_text
    copy2 = shutil.copy2

    def substituted(candidate, *args, **kwargs):
        if path is not None and candidate.resolve() == path.resolve():
            return source
        return read_text(candidate, *args, **kwargs)

    def copied(candidate, destination, *args, **kwargs):
        if path is not None and Path(candidate).resolve() == path.resolve():
            # Only the unit test's small temporary apparatus mirror is changed.
            Path(destination).write_text(source)
            return str(destination)
        return copy2(candidate, destination, *args, **kwargs)

    stream = io.StringIO()
    with patch.object(Path, 'read_text', substituted), patch.object(shutil, 'copy2', copied):
        result = unittest.TextTestRunner(stream=stream, verbosity=2,
                                        resultclass=AssertionResult).run(
            unittest.TestSuite([module.BootstrapTests(method)]))
    return result, stream.getvalue()


def bootstrap_checks(apparatus, rows, output, selection):
    module = load('test_cli_parity', apparatus / 'test_cli_parity.py')
    selected = [row for row in bootstrap_regressions(apparatus)
                if not selection or row[0] == selection]
    if not selected:
        record(rows, selection, 'INVALID', 'unknown bootstrap regression name')
        return
    for name, path, changed, method, assertion in selected:
        test = 'test_cli_parity.BootstrapTests.' + method
        status, reason, compiled = 'INVALID', '', False
        logs = []
        try:
            positive, log = run_bound_test(module, method)
            logs.append('POSITIVE\n' + log)
            if not passed(positive):
                raise RuntimeError('bound positive test did not pass')
            if changed == path.read_text():
                raise RuntimeError('stale mutation anchor; source did not change')
            if path.suffix == '.py':
                compile(changed, str(path), 'exec')
                compiled = True
                logs.append('COMPILE: Python source accepted\n')
            else:
                logs.append('COMPILE: not applicable to Markdown command mutation\n')
            negative, log = run_bound_test(module, method, path, changed)
            logs.append('NEGATIVE\n' + log)
            status, reason = classify(negative, test, assertion)
        except Exception as error:
            status, reason = 'INVALID', f'{type(error).__name__}: {error}'
        try:
            restored, log = run_bound_test(module, method)
            logs.append('RESTORED (memory substitution removed)\n' + log)
            if not passed(restored):
                status, reason = 'INVALID', reason + '; restored bound test did not pass'
        except Exception as error:
            status, reason = 'INVALID', reason + f'; restored: {type(error).__name__}: {error}'
        if output:
            with (output / (name.replace('/', '-') + '.log')).open('x') as stream:
                stream.write(''.join(logs))
        else:
            print(''.join(logs), flush=True)
        record(rows, name, status, reason)
        rows[-1].update(test=test, assertion=assertion, compiled=compiled,
                        mutationKind=('temporary unit-test mirror' if method.startswith(('test_each_entry', 'test_discover')) else
                                      'in-memory AST input' if path.suffix == '.py' else 'in-memory document'))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--apparatus', type=Path, default=DEFAULT_APPARATUS,
                        help='apparatus directory to exercise (default: this checkout)')
    parser.add_argument('--output', type=Path,
                        help='directory for summary.json (optional)')
    parser.add_argument('--bootstrap-only', action='store_true',
                        help='only run bootstrap/document regressions')
    parser.add_argument('--case', help='one bootstrap regression name (requires --bootstrap-only)')
    args = parser.parse_args()
    if args.case and not args.bootstrap_only:
        parser.error('--case requires --bootstrap-only')
    apparatus = args.apparatus.resolve()
    missing = [name for name in MIRROR_FILES + ('README.md',)
               if not (apparatus / name).is_file()]
    if missing:
        print(json.dumps({'status': 'BLOCKED', 'error': f'missing apparatus files: {missing}'}))
        return 2
    stale = [str(apparatus / name) for name in ('json.py', 'common', 'policy', 'compare')
             if (apparatus / name).exists()]
    suffix = sysconfig.get_config_var('EXT_SUFFIX')
    if suffix and (apparatus / f'common{suffix}').exists():
        stale.append(str(apparatus / f'common{suffix}'))
    if stale:
        print(json.dumps({'status': 'BLOCKED', 'error': f'stale imposter paths: {stale}'}))
        return 2
    rows = []
    if args.output:
        args.output.mkdir(parents=True, exist_ok=False)
    try:
        bootstrap_checks(apparatus, rows, args.output, args.case)
        if not args.bootstrap_only:
            static_checks(apparatus, rows)
            with tempfile.TemporaryDirectory(prefix='pdrs-cli-parity-neg-') as temp:
                holder = Path(temp)
                mirror = holder / 'apparatus'
                mirror.mkdir()
                for name in MIRROR_FILES:
                    shutil.copy2(apparatus / name, mirror / name)
                for check in (no_isolated_checks, package_checks, extension_check):
                    try:
                        check(mirror, rows, holder)
                    except Exception as error:
                        record(rows, check.__name__, 'INVALID', f'{type(error).__name__}: {error}')
    except BaseException as error:
        if isinstance(error, KeyboardInterrupt):
            raise
        print(json.dumps({'status': 'BLOCKED', 'error': f'{type(error).__name__}: {error}'}))
        return 2
    counts = {state: sum(row['status'] == state for row in rows)
              for state in ('KILLED', 'SURVIVED', 'INVALID')}
    summary = {'apparatus': str(apparatus), 'controls': rows, 'counts': counts}
    if args.output:
        (args.output / 'summary.json').write_text(json.dumps(summary, indent=2) + '\n')
    print(json.dumps({'apparatus': str(apparatus), 'counts': counts}))
    return 0 if counts['KILLED'] == len(rows) and rows else 1


if __name__ == '__main__':
    raise SystemExit(main())
