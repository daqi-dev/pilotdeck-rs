#!/usr/bin/env python3
# anchor-selfcheck: unsupported environment-level imposter files and subprocess isolation checks, no source replacement table
"""#701 item 3 negative controls: cli-parity entries must ignore same-dir imposters.

Each row mutates the *environment* beside the apparatus (a same-named package
directory, or a stdlib module file that only ``-I`` keeps out of ``sys.path``)
and must be KILLED: the entry loads the real apparatus file or exits at the
isolated guard, and the imposter never executes.  SURVIVED means the pre-fix
behaviour came back; INVALID means the check could not run (stale path, syntax
error, or a witness that failed to exercise the resolver).

The in-place window holds ``common.parity_lock()`` so no calibration round can
observe a half-written apparatus.  Every imposter is removed in a ``finally``;
a leftover at startup refuses the run instead of guessing.  Run all rows and
summarise; any non-KILLED row is a nonzero exit.
"""
import argparse
import ast
import importlib.util
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import sysconfig
import tempfile

ROOT = Path(__file__).resolve().parents[5]
DEFAULT_APPARATUS = ROOT / 'scripts/cli-parity'
ENTRY_FILES = ('compare.py', 'controls.py', 'host_negative_controls.py',
               'run_round.py', 'test_cli_parity.py')
GUARDED_FILES = ENTRY_FILES + ('policy.py',)
APPARATUS_MODULES = ('common', 'policy', 'compare', 'controls', 'run_round',
                     'host_negative_controls')
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


def load(name, path):
    path = Path(path).resolve()
    existing = sys.modules.get(name)
    existing_file = getattr(existing, '__file__', None)
    if existing is not None and existing_file and Path(existing_file).resolve() == path:
        if not isinstance(getattr(existing, '_apparatus_source', None), bytes):
            raise RuntimeError(f'unverified apparatus module cache: {path}')
        return existing
    spec = importlib.util.spec_from_file_location(name, path)
    module = importlib.util.module_from_spec(spec)
    sys.modules[name] = module
    try:
        source = path.read_bytes()
        exec(compile(source, str(path), 'exec'), module.__dict__)
    except BaseException:
        sys.modules.pop(name, None)
        raise
    module._apparatus_source = source
    if Path(module.__file__).resolve() != path:
        raise RuntimeError(f'{name}: loaded {module.__file__}, expected {path}')
    return module


COMMON = load('common', DEFAULT_APPARATUS / 'common.py')


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
    if ast.unparse(guard.test) != 'not sys.flags.isolated':
        return f'guard tests {ast.unparse(guard.test)!r}, not "not sys.flags.isolated"'
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
    return subprocess.run(command, cwd=str(ROOT), env=env, capture_output=True,
                          text=True, timeout=timeout)


def record(rows, name, status, reason):
    rows.append({'name': name, 'status': status, 'reason': reason})
    print(f'{name}: {status}: {reason}', flush=True)


def static_checks(apparatus, rows):
    for name in GUARDED_FILES + ('common.py',):
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


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--apparatus', type=Path, default=DEFAULT_APPARATUS,
                        help='apparatus directory to exercise (default: this checkout)')
    parser.add_argument('--output', type=Path,
                        help='directory for summary.json (optional)')
    args = parser.parse_args()
    apparatus = args.apparatus.resolve()
    missing = [name for name in ('common.py',) + ENTRY_FILES + ('scenarios.json',)
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
    try:
        with COMMON.parity_lock():
            holder = Path(tempfile.mkdtemp(prefix='pdrs-cli-parity-neg-'))
            try:
                static_checks(apparatus, rows)
                no_isolated_checks(apparatus, rows, holder)
                package_checks(apparatus, rows, holder)
                extension_check(apparatus, rows, holder)
            finally:
                shutil.rmtree(holder, ignore_errors=True)
    except BaseException as error:
        if isinstance(error, KeyboardInterrupt):
            raise
        print(json.dumps({'status': 'BLOCKED', 'error': f'{type(error).__name__}: {error}'}))
        return 2
    counts = {state: sum(row['status'] == state for row in rows)
              for state in ('KILLED', 'SURVIVED', 'INVALID')}
    summary = {'apparatus': str(apparatus), 'controls': rows, 'counts': counts}
    if args.output:
        args.output.mkdir(parents=True, exist_ok=True)
        (args.output / 'summary.json').write_text(json.dumps(summary, indent=2) + '\n')
    print(json.dumps({'apparatus': str(apparatus), 'counts': counts}))
    return 0 if counts['KILLED'] == len(rows) and rows else 1


if __name__ == '__main__':
    raise SystemExit(main())
