#!/usr/bin/env python3
"""Host-only negative: a native candidate execv's Node under the Rust policy."""
import sys
import os

# -I keeps PYTHONPATH, user site and the script directory out of module
# resolution.  Without it, exit before importing anything: a same-named stdlib
# file beside this entry (json.py, pathlib.py, tempfile.py) would otherwise
# execute before the guard. Discover can add the directory back even with
# -I, so reject that path too. Both boot imports are safe: ``sys`` is builtin
# and ``os`` has been frozen since CPython 3.11, so neither can be shadowed
# from the reinserted directory.
# The directory is identified by (device, inode), not by its spelling: a
# case-insensitive alias or a mixed ``//``/symlink form names this very
# directory while the raw string differs (fix-r2 GLM sibling P1).
_boot_dir = os.path.dirname(os.path.realpath(__file__))
_boot_reinserted = any(
    os.path.isdir(entry or os.curdir)
    and os.path.samefile(entry or os.curdir, _boot_dir)
    for entry in sys.path)
if not sys.flags.isolated or _boot_reinserted:
    raise SystemExit(f'#701: unsafe import path; run with -I by file: python3 -I {__file__}')

import importlib.util
from pathlib import Path

# The bootstrap itself is also loaded from source by its exact sibling path.
_bootstrap_path = Path(__file__).resolve().with_name('_bootstrap.py')
_bootstrap_spec = importlib.util.spec_from_file_location('_cli_parity_bootstrap', _bootstrap_path)
_bootstrap = importlib.util.module_from_spec(_bootstrap_spec)
exec(compile(_bootstrap_path.read_bytes(), str(_bootstrap_path), 'exec'), _bootstrap.__dict__)
_load = _bootstrap.load

import argparse
import json
import subprocess

_here = Path(__file__).resolve().parent
_common = _load('common', _here / 'common.py')
ROOT = _common.ROOT
SCRIPT = _common.SCRIPT
SCENARIOS = _common.SCENARIOS
baseline_path = _common.baseline_path
output_path = _common.output_path
read_plain = _common.read_plain
save = _common.save
select_case = _load('controls', _here / 'controls.py').select_case


def check_exec_control(recording, summary, runner_exit):
    scenario = select_case({row['id']: row for row in SCENARIOS}, 'help-long', lambda case: True)
    if scenario is None:
        return {'status': 'BLOCKED', 'control': 'native-execv-node-as-rust',
                'scenario': None, 'reason': 'current suite has no case to execute'}
    case = recording / 'rust/cases' / scenario
    passed = (runner_exit == 1 and summary['status'] == 'FAIL'
              and summary.get('rustComparison', {}).get('status') == 'FAIL'
              and read_plain(case / 'stderr.bin') == b'D0 exec-node denied\n'
              and json.loads(read_plain(case / 'record.json'))['exitCode'] == 126)
    return {'status': 'PASS' if passed else 'BLOCKED' if summary['status'] == 'BLOCKED' else 'FAIL',
            'control': 'native-execv-node-as-rust', 'scenario': scenario, 'runnerExit': runner_exit}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--baseline', required=True, type=Path)
    parser.add_argument('--node', required=True, type=Path)
    parser.add_argument('--output', required=True, type=Path)
    args = parser.parse_args()
    output = output_path(args.output)
    output.mkdir(parents=True, mode=0o700)
    baseline, node = baseline_path(args.baseline), args.node.resolve()
    binary = output / 'exec-node-candidate'
    compile_argv = ['/usr/bin/clang', '-Wall', '-Wextra', '-Werror',
                    '-DNODE_PATH=' + json.dumps(str(node)),
                    '-DTS_ENTRY=' + json.dumps(str(baseline / 'dist/src/cli/pilotdeck.js')),
                    str(SCRIPT / 'exec_node_control.c'), '-o', str(binary)]
    build = subprocess.run(compile_argv, capture_output=True, timeout=60)
    (output / 'build.stdout.bin').write_bytes(build.stdout)
    (output / 'build.stderr.bin').write_bytes(build.stderr)
    if build.returncode:
        raise RuntimeError('installed clang could not build native execv negative control')
    argv = [sys.executable, '-I', str(SCRIPT / 'run_round.py'), '--baseline', str(baseline),
            '--node', str(node), '--rust-bin', str(binary), '--rust-source', str(ROOT),
            '--output', str(output / 'recording')]
    with (output / 'runner.stdout.log').open('wb') as stdout, (output / 'runner.stderr.log').open('wb') as stderr:
        result = subprocess.run(argv, stdout=stdout, stderr=stderr, timeout=2380)
    # Evidence leaves are read like compare.py: plain single-link files only.
    summary = json.loads(read_plain(output / 'recording/summary.json'))
    report = {**check_exec_control(output / 'recording', summary, result.returncode),
              'compileArgv': compile_argv, 'runnerArgv': argv, 'summary': summary}
    save(output / 'control.json', report)
    print(json.dumps(report, ensure_ascii=False, indent=2))
    return {'PASS': 0, 'FAIL': 1, 'BLOCKED': 2}[report['status']]


if __name__ == '__main__':
    raise SystemExit(main())
