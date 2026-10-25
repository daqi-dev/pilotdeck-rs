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

# Never read a repository __pycache__ before the scan: a crafted unchecked-hash
# pyc executes on import and could subvert the checker itself (fix-r5).  -B only
# stops writes, so redirect the bytecode search to a fresh temp prefix as well.
sys.pycache_prefix = tempfile.mkdtemp(prefix='pdrs-gateway-pyc-')
sys.dont_write_bytecode = True

# python3 -I removes the script directory from sys.path; the apparatus directory
# is the only trusted import root.
sys.path.insert(0, str(Path(__file__).resolve().parent))

from common import ROOT, cargo_config_problems


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
