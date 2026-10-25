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
import argparse
import json
from pathlib import Path
import subprocess
import sys
import tempfile

# >>> apparatus bootstrap (verbatim in every A2 entry; test_safety checks it)
# Load apparatus modules by explicit path, never sys.path: a same-named package
# or extension module beside the source would win before the scan (fix-r6 P1).
# Inline on purpose: importing common.py is what this block defends.  -B only
# stops cache writes, so redirect the bytecode prefix too (fix-r5).
import importlib.util, sys, tempfile
from pathlib import Path

# -I keeps PYTHONPATH, user site and the script directory out of module
# resolution.  Without it, exit before loading anything and print the command.
if not sys.flags.isolated:
    raise SystemExit(f'run with -I: python3 -I {Path(__file__).resolve()}')

sys.pycache_prefix = tempfile.mkdtemp(prefix='pdrs-gateway-pyc-')
sys.dont_write_bytecode = True


def _load(name, path):
    path = Path(path).resolve()
    existing = sys.modules.get(name)
    existing_file = getattr(existing, '__file__', None)
    if existing is not None and existing_file and Path(existing_file).resolve() == path:
        return existing
    spec = importlib.util.spec_from_file_location(name, path)
    module = importlib.util.module_from_spec(spec)
    sys.modules[name] = module
    spec.loader.exec_module(module)
    if Path(module.__file__).resolve() != path:
        raise RuntimeError(f'{name}: loaded {module.__file__}, expected {path}')
    return module


_here = Path(__file__).resolve().parent
_apparatus = _here.parent if _here.name == 'controls' else _here
common = _load('common', _apparatus / 'common.py')
# <<< apparatus bootstrap

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
        return 1
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
        return 1


if __name__ == '__main__':
    raise SystemExit(main())
