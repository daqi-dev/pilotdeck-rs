#!/usr/bin/env python3
"""Five mutations of a complete real TS recording; no synthetic calibration fallback."""
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
import shutil
import tempfile

_here = Path(__file__).resolve().parent
_common = _load('common', _here / 'common.py')
output_path = _common.output_path
read_plain = _common.read_plain
save = _common.save
snapshot = _common.snapshot
_compare = _load('compare', _here / 'compare.py')
compare = _compare.compare
load_round = _compare.load_round
plain = _compare.plain


def select_case(cases, preferred, eligible):
    """Keep calibrated choices when eligible, otherwise use recording order."""
    if preferred in cases and eligible(cases[preferred]):
        return preferred
    return next((name for name, case in cases.items() if eligible(case)), None)


def disk_file(case):
    files = [name for name, row in case['disk'].items() if row['type'] == 'file']
    return 'project/keep.txt' if 'project/keep.txt' in files else next(iter(sorted(files)), None)


def run_controls(source, output):
    source, output = Path(source).absolute(), Path(output).absolute()
    if output != output.resolve() or source.is_relative_to(output) or output.is_relative_to(source):
        raise ValueError('source and controls output must be disjoint non-symlink trees')
    valid = compare(source, source)
    if valid['status'] != 'PASS':
        return {'status': 'BLOCKED', 'error': 'controls require complete, valid TS evidence', 'validation': valid}
    output.mkdir(parents=True, exist_ok=False)
    results = []
    _, cases, errors = load_round(source)
    if errors:
        return {'status': 'BLOCKED', 'error': 'controls source changed after validation', 'validation': errors}
    specs = [('drop-stdout', 'help-long', 'stdout', lambda case: bool(case['stdout'])),
             ('change-stderr', 'flag-missing', 'stderr', lambda case: case['stderr'] != b'INJECTED STDERR\n'),
             ('change-exit', 'help-long', 'exit', lambda case: True),
             ('drop-disk', 'help-long', 'disk', lambda case: disk_file(case) is not None),
             ('extra-file', 'help-long', 'disk', lambda case: disk_file(case) is not None)]
    for name, preferred, surface, eligible in specs:
        scenario = select_case(cases, preferred, eligible)
        if scenario is None:
            results.append({'name': name, 'status': 'NOT_APPLICABLE', 'scenario': None,
                            'reason': f'current suite has no eligible case for {name} ({surface})'})
            continue
        target = output / name
        shutil.copytree(source, target, symlinks=True)
        case = target / 'cases' / scenario
        # Defense in depth: copy validation precedes all mutation, including metadata.
        for filename in ('stdout.bin', 'stderr.bin', 'record.json', 'events.json', 'final.json'):
            plain(case / filename)
        if surface in ('stdout', 'stderr'):
            data = b'' if name == 'drop-stdout' else b'INJECTED STDERR\n'
            (case / f'{surface}.bin').write_bytes(data)
            # Keep event/raw coherence so semantic comparison, not stale cache, catches it.
            events = json.loads(read_plain(case / 'events.json'))
            events = [row for row in events if row['stream'] != surface]
            if data:
                events.append({'stream': surface, 'hex': data.hex()})
            save(case / 'events.json', events)
        elif surface == 'exit':
            record = json.loads(read_plain(case / 'record.json'))
            record['exitCode'] = 73 if record['exitCode'] != 73 else 74
            save(case / 'record.json', record)
        else:
            artifact = case / 'tree' / disk_file(cases[scenario])
            plain(artifact)
            if name == 'drop-disk':
                artifact.unlink()
            else:
                extra = artifact.with_name('unexpected.bin')
                while extra.exists() or extra.is_symlink():
                    extra = extra.with_name(extra.name + '.extra')
                with extra.open('xb') as stream:
                    stream.write(b'INJECTED\x00\xff')
            save(case / 'final.json', snapshot(case / 'tree'))
        result = compare(source, target)
        rules = [row['rule'] for row in result['failures']]
        expected = f'{scenario}.oracle.{surface}'
        pair_rule = f'{scenario}.{"exitCode" if surface == "exit" else surface}'
        passed = result['status'] == 'FAIL' and expected in rules and pair_rule in rules
        save(output / (name + '.comparison.json'), result)
        results.append({'name': name, 'status': 'PASS' if passed else 'FAIL',
                        'scenario': scenario, 'passed': passed, 'expectedRule': expected,
                        'expectedPairRule': pair_rule, 'observedRules': rules})
    report = {'status': ('FAIL' if any(row['status'] == 'FAIL' for row in results)
                         else 'PASS' if any(row['status'] == 'PASS' for row in results) else 'BLOCKED'),
              'source': str(source), 'controls': results,
              'notApplicable': [row['name'] for row in results if row['status'] == 'NOT_APPLICABLE']}
    save(output / 'controls.json', report)
    return report


if __name__ == '__main__':
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument('--source', required=True, type=Path)
    ap.add_argument('--output', required=True, type=Path)
    args = ap.parse_args()
    try:
        result = run_controls(args.source, output_path(args.output))
    except (OSError, ValueError, RuntimeError) as error:
        result = {'status': 'BLOCKED', 'error': str(error)}
    print(json.dumps(result, ensure_ascii=False, indent=2))
    raise SystemExit({'PASS': 0, 'FAIL': 1, 'BLOCKED': 2}[result['status']])
