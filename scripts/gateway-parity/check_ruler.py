#!/usr/bin/env python3
"""k=5 TS calibration, per-rule controls, same-bad, independent review=null.

No excluded scenarios or tolerance. Reuses existing archives without changing
evidence; choose a fresh --output to rerun. A pending review returns exit 2.
"""
import argparse
from contextlib import redirect_stdout
import importlib.util
from itertools import combinations
import json
from pathlib import Path
import sys

from common import DEFAULT_OUT, SCRIPT, apparatus_hashes, install_shutdown, parity_lock, save
from compare import compare
from run_round import run_round


def main():
    install_shutdown()
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument('--output', type=Path, default=DEFAULT_OUT)
    ap.add_argument('--baseline', type=Path)
    ap.add_argument('--lock-timeout', type=float, default=600)
    args = ap.parse_args()
    output = args.output.resolve()
    hashes = apparatus_hashes()
    takeovers = []
    with parity_lock(args.lock_timeout, takeovers):
        for label in ['ts1', 'ts2', 'ts3', 'ts4', 'ts5', 'proxy']:
            if (output / label).exists():
                archived = json.loads((output / label / 'run.json').read_text())
                if archived.get('apparatusSha256') != hashes:
                    raise ValueError(f'{label}: apparatus hash mismatch; choose a fresh --output')
            else:
                with redirect_stdout(sys.stderr):
                    run_round('rust' if label == 'proxy' else 'ts', label, output, args.baseline)
        identities = []
        distinct = True
        for index in range(1, 6):
            folder = output / f'ts{index}'
            try:
                run = json.loads((folder / 'run.json').read_text())
                processes = json.loads((folder / 'processes.json').read_text())
                pid = next(row['pid'] for row in processes if row['name'] == 'gateway')
                distinct = distinct and run['side'] == 'ts' and run['label'] == f'ts{index}'
                identities.append(pid)
            except (OSError, ValueError, TypeError, KeyError, StopIteration):
                distinct = False
        distinct = distinct and len(set(identities)) == 5
        comparisons = []
        for a, b in combinations(range(1, 6), 2):
            result = compare(output / f'ts{a}', output / f'ts{b}')
            save(output / f'compare-ts{a}-ts{b}.json', result)
            comparisons.append({'pair': [a, b], 'status': result['status'], 'rules': [row['rule'] for row in result['failures']]})
        proxy = compare(output / 'ts1', output / 'proxy')
        save(output / 'compare-proxy.json', proxy)
        spec = importlib.util.spec_from_file_location('gateway_controls', SCRIPT / 'controls/run_all.py')
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
        controls_path = output / 'controls'
        # Recompute in a new directory so a cached successful control report
        # cannot hide subsequent edits to source evidence or comparator.
        index = 1
        while controls_path.exists():
            controls_path = output / f'controls-{index}'
            index += 1
        controls = module.run_controls(output / 'ts1', controls_path, output / 'proxy')
        cases = controls.get('cases', {})
        report = {
            'k': 5, 'pairs': comparisons, 'apparatusSha256': hashes,
            'self_compare': distinct and all(item['status'] == 'PASS' for item in comparisons),
            'distinct_ts_rounds': distinct, 'gateway_pids': identities,
            'positive_controls': bool(cases) and all(item['passed'] for name, item in cases.items() if not item.get('bothSides')),
            'same_bad_rejected': bool(cases) and all(cases.get(name, {}).get('passed', False) for name in
                                                  ('both-sides-bad', 'corrupt-fingerprint', 'both-bad-disk', 'both-bad-http', 'both-bad-wire-metadata')),
            'independent_review': None,
            'proxy_parity': proxy['status'] == 'PASS',
            'proxy_rules': [item['rule'] for item in proxy['failures']],
            'controls': str(controls_path / 'results.json'), 'lock_takeovers': takeovers,
            'status': 'PENDING_REVIEW',
        }
        if not all(report[key] for key in ('self_compare', 'positive_controls', 'same_bad_rejected', 'proxy_parity')):
            report['status'] = 'FAIL'
        if apparatus_hashes() != hashes:
            raise ValueError('apparatus changed during run; report cannot certify mixed versions')
        save(output / 'ruler.json', report)
    print(json.dumps(report, ensure_ascii=False, indent=2))
    return 1 if report['status'] == 'FAIL' else 2


if __name__ == '__main__':
    try:
        code = main()
    except Exception as error:
        print(json.dumps({'status': 'BLOCKED', 'error': str(error)}, ensure_ascii=False))
        code = 1
    raise SystemExit(code)
