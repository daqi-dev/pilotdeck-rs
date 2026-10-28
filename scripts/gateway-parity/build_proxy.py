#!/usr/bin/env python3
"""Build the gateway proxy with reviewable cargo inputs only (#85 85.1).

A cargo build reads config from $CARGO_HOME and from .cargo/config[.toml] in the
working directory and every ancestor. Untracked worktree configs and persistent
CARGO_HOME configs can change build flags without appearing in review, so this
entry refuses them before invoking cargo. ``--check`` runs the preflight alone.

Boundary: this wrapper cannot prove how a bypassing ``cargo build`` was invoked,
and it does not hash CARGO_HOME/registry source trees; it does pin the lockfile
via ``--locked`` and refuses the two configurable inputs named by issue #85.
"""
# >>> apparatus bootstrap (verbatim in every A2 entry; test_safety checks it)
# -I keeps PYTHONPATH, user site and the script directory out of module
# resolution.  It must run before every other standard-library import: a
# shadowed argparse/pathlib would otherwise execute before this check.  ``sys``
# is already initialised by the interpreter, so it is the only safe import.
import sys

if not sys.flags.isolated:
    raise SystemExit(f'run with -I: python3 -I {__file__}')

import importlib.util
from pathlib import Path
import tempfile

# -B only stops cache writes, so redirect the bytecode prefix too (fix-r5).
sys.pycache_prefix = tempfile.mkdtemp(prefix='pdrs-gateway-pyc-')
sys.dont_write_bytecode = True
# CPython has already compiled the CLI entry before it can acquire a lease.
# Capture that executing object, not a later read of the restored entry file.
_apparatus_code = sys._getframe().f_code

# Load the one guard by explicit path, never sys.path: a same-named package or
# extension module beside the source would win before the scan (fix-r6 P1).
# Inline on purpose: importing the guard is what this block defends.
_here = Path(__file__).resolve().parent
_apparatus = _here.parent if _here.name == 'controls' else _here
_guard_path = _apparatus / '_guard.py'
_guard = sys.modules.get('_guard')
if not (getattr(_guard, '__file__', None)
        and Path(_guard.__file__).resolve() == _guard_path
        and isinstance(getattr(_guard, '_apparatus_source', None), bytes)):
    _spec = importlib.util.spec_from_file_location('_guard', _guard_path)
    _guard = importlib.util.module_from_spec(_spec)
    sys.modules['_guard'] = _guard
    _guard_source = _guard_path.read_bytes()
    try:
        exec(compile(_guard_source, str(_guard_path), 'exec'), _guard.__dict__)
    except BaseException:
        sys.modules.pop('_guard', None)
        raise
    _guard._apparatus_source = _guard_source
if Path(_guard.__file__).resolve() != _guard_path:
    raise RuntimeError(f'_guard: loaded {_guard.__file__}, expected {_guard_path}')

_load = _guard.load
_has_git_checkout = _guard._has_git_checkout
_verify_cli_imports = _guard.verify_cli_imports
run_entry = _guard.run_entry
common = _load('common', _apparatus / 'common.py')
# <<< apparatus bootstrap
import argparse
import json
from pathlib import Path
import subprocess
import sys
import tempfile


ROOT = common.ROOT
cargo_config_problems = common.cargo_config_problems


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--check', action='store_true', help='preflight only, do not build')
    parser.add_argument('--cargo', default='cargo', help='cargo executable')
    parser.add_argument('--target-dir', type=Path, help='optional CARGO_TARGET_DIR')
    args = parser.parse_args()
    problems = cargo_config_problems()
    if problems:
        print(json.dumps({'status': 'BLOCKED', 'cargoConfigProblems': problems},
                         ensure_ascii=False))
        return 2
    if args.check:
        print(json.dumps({'status': 'PASS', 'cargoConfigProblems': []}, ensure_ascii=False))
        return 0
    command = [args.cargo, 'build', '--manifest-path', 'src/Cargo.toml',
               '-p', 'pilotdeck-gateway', '--locked']
    if args.target_dir:
        command += ['--target-dir', str(args.target_dir)]
    print(json.dumps({'status': 'BUILD', 'command': command}, ensure_ascii=False), flush=True)
    try:
        return subprocess.call(command, cwd=ROOT)
    except OSError as error:
        print(json.dumps({'status': 'BLOCKED', 'error': str(error)}, ensure_ascii=False))
        return 2


if __name__ == '__main__':
    run_entry(main, 'traceback')
