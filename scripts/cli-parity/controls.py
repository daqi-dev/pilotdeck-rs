#!/usr/bin/env python3
"""Five mutations of a complete real TS recording; no synthetic calibration fallback."""
import argparse
import json
from pathlib import Path
import shutil
import sys

sys.dont_write_bytecode = True
sys.path.insert(0, str(Path(__file__).resolve().parent))
from common import output_path, read_plain, save, snapshot
from compare import compare, plain


def run_controls(source, output):
    source, output = Path(source).absolute(), Path(output).absolute()
    if output != output.resolve() or source.is_relative_to(output) or output.is_relative_to(source):
        raise ValueError('source and controls output must be disjoint non-symlink trees')
    valid = compare(source, source)
    if valid['status'] != 'PASS':
        return {'status': 'BLOCKED', 'error': 'controls require complete, valid TS evidence', 'validation': valid}
    output.mkdir(parents=True, exist_ok=False)
    results = []
    specs = [('drop-stdout', 'help-long', 'stdout'),
             ('change-stderr', 'flag-missing', 'stderr'),
             ('change-exit', 'help-long', 'exit'),
             ('drop-disk', 'help-long', 'disk'),
             ('extra-file', 'help-long', 'disk')]
    for name, scenario, surface in specs:
        target = output / name
        shutil.copytree(source, target, symlinks=True)
        case = target / 'cases' / scenario
        # Defense in depth: copy validation precedes all mutation, including metadata.
        for filename in ('stdout.bin', 'stderr.bin', 'record.json', 'events.json', 'final.json'):
            plain(case / filename)
        plain(case / 'tree/project/keep.txt')
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
            record['exitCode'] = 73
            save(case / 'record.json', record)
        else:
            if name == 'drop-disk':
                (case / 'tree/project/keep.txt').unlink()
            else:
                (case / 'tree/project/unexpected.bin').write_bytes(b'INJECTED\x00\xff')
            save(case / 'final.json', snapshot(case / 'tree'))
        result = compare(source, target)
        rules = [row['rule'] for row in result['failures']]
        expected = f'{scenario}.oracle.{surface}'
        pair_rule = f'{scenario}.{"exitCode" if surface == "exit" else surface}'
        passed = result['status'] == 'FAIL' and expected in rules and pair_rule in rules
        save(output / (name + '.comparison.json'), result)
        results.append({'name': name, 'passed': passed, 'expectedRule': expected,
                        'expectedPairRule': pair_rule, 'observedRules': rules})
    report = {'status': 'PASS' if all(row['passed'] for row in results) else 'FAIL',
              'source': str(source), 'controls': results}
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
