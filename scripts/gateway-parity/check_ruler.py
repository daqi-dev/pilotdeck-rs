#!/usr/bin/env python3
"""k=5 TS calibration, per-rule controls, same-bad, independent review=null.

No excluded scenarios or tolerance. Reuses existing archives without changing
evidence; choose a fresh --output to rerun. A pending review returns exit 2.
"""
import argparse
from contextlib import redirect_stdout
from itertools import combinations
import json
import os
from pathlib import Path
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
# CPython has already compiled the CLI entry before it can acquire a lease.
# Capture that executing object, not a later read of the restored entry file.
_apparatus_code = sys._getframe().f_code


def _load(name, path):
    path = Path(path).resolve()
    existing = sys.modules.get(name)
    existing_file = getattr(existing, '__file__', None)
    if existing is not None and existing_file and Path(existing_file).resolve() == path:
        if not isinstance(getattr(existing, '_apparatus_source', None), bytes):
            raise RuntimeError(f'#166 unverified apparatus module cache: {path}')
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



def _verify_cli_imports():
    # This trust root uses only stdlib + Git, never a helper from disk common:
    # that module's verifier can itself be the active negative-control mutant.
    import subprocess
    from types import ModuleType
    root = _apparatus.parent.parent
    head = subprocess.check_output(['git', '-C', str(root), 'rev-parse', '--verify', 'HEAD'],
                                   timeout=30).decode().strip()
    source = subprocess.check_output(['git', '-C', str(root), 'show',
                                      f'{head}:scripts/gateway-parity/common.py'], timeout=30)
    trusted = ModuleType('pristine_cli_verifier')
    trusted.__file__ = str(_apparatus / 'common.py')
    exec(compile(source, trusted.__file__, 'exec'), trusted.__dict__)
    trusted.verify_loaded_apparatus(head)


_here = Path(__file__).resolve().parent
_apparatus = _here.parent if _here.name == 'controls' else _here
common = _load('common', _apparatus / 'common.py')
# <<< apparatus bootstrap

DEFAULT_OUT = common.DEFAULT_OUT
ROOT = common.ROOT
SCRIPT = common.SCRIPT
apparatus_hashes = common.apparatus_hashes
baseline_dependency_hashes = common.baseline_dependency_hashes
cargo_config_problems = common.cargo_config_problems
digest = common.digest
install_shutdown = common.install_shutdown
parity_lock = common.verified_parity_lock
save = common.save
compare_module = _load('compare', common.SCRIPT / 'compare.py')
compare = compare_module.compare
run_round_module = _load('run_round', common.SCRIPT / 'run_round.py')
run_round = run_round_module.run_round


def main():
    install_shutdown()
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument('--output', type=Path, default=DEFAULT_OUT)
    ap.add_argument('--baseline', type=Path)
    ap.add_argument('--lock-timeout', type=float, default=600)
    args = ap.parse_args()
    output = args.output.resolve()
    baseline = Path(args.baseline or os.environ.get('PDRS_BASELINE')
                    or ROOT / 'resources/PilotDeck').resolve()
    takeovers = []
    with parity_lock(args.lock_timeout, takeovers):
        problems = cargo_config_problems()
        if problems:
            raise ValueError('BLOCKED: cargo build inputs are not frozen: ' + '; '.join(problems))
        dependencies = baseline_dependency_hashes(baseline)
        hashes = apparatus_hashes()
        for label in ['ts1', 'ts2', 'ts3', 'ts4', 'ts5', 'proxy']:
            if (output / label).exists():
                archived = json.loads((output / label / 'run.json').read_text())
                if archived.get('apparatusSha256') != hashes:
                    raise ValueError(f'{label}: apparatus hash mismatch; choose a fresh --output')
                if archived.get('baselineDepsSha256') != dependencies:
                    raise ValueError(f'{label}: baseline dependency hash mismatch; choose a fresh --output')
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
        module = _load('gateway_controls', SCRIPT / 'controls/run_all.py')
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
            'baselineDepsSha256': dependencies,
            'self_compare': distinct and all(item['status'] == 'PASS' for item in comparisons),
            'distinct_ts_rounds': distinct, 'gateway_pids': identities,
            'positive_controls': bool(cases) and all(item['passed'] for name, item in cases.items() if not item.get('bothSides')),
            'prerequisite_controls': controls.get('prerequisites', {}).get('passed', False),
            # Skips stay outside the pass count and are listed explicitly; a
            # declared host-degenerate skip is allowed, an undeclared one fails
            # prerequisite_controls via summarize_results() (fix-r6 P2).
            'prerequisite_skips': controls.get('prerequisites', {}).get('skipped', {}),
            'prerequisite_skipped_count': controls.get('prerequisites', {}).get('skippedCount', 0),
            'prerequisite_unexpected_skips': controls.get('prerequisites', {}).get('unexpectedSkips', []),
            'prerequisiteSha256': controls.get('prerequisites', {}).get('inputSha256', {}),
            'prerequisiteAuditSha256': controls.get('prerequisites', {}).get('auditSha256'),
            'same_bad_rejected': bool(cases) and all(cases.get(name, {}).get('passed', False) for name in
                                                  ('both-sides-bad', 'corrupt-fingerprint', 'both-bad-disk', 'both-bad-http', 'both-bad-wire-metadata')),
            'independent_review': None,
            'proxy_parity': proxy['status'] == 'PASS',
            'proxy_rules': [item['rule'] for item in proxy['failures']],
            'controls': str(controls_path / 'results.json'), 'lock_takeovers': takeovers,
            'status': 'PENDING_REVIEW',
        }
        if not all(report[key] for key in ('self_compare', 'positive_controls', 'prerequisite_controls', 'same_bad_rejected', 'proxy_parity')):
            report['status'] = 'FAIL'
        if apparatus_hashes() != hashes:
            raise ValueError('apparatus changed during run; report cannot certify mixed versions')
        if baseline_dependency_hashes(baseline) != dependencies:
            raise ValueError('baseline dependencies changed during run; report cannot certify mixed versions')
        if any(digest(ROOT / name) != expected for name, expected in report['prerequisiteSha256'].items()):
            raise ValueError('prerequisite inputs changed after controls; report cannot certify mixed versions')
        if digest(ROOT / 'docs/reviews/002_a4-prerequisites.md') != report['prerequisiteAuditSha256']:
            raise ValueError('audit artifact changed after controls; report cannot certify mixed versions')
        save(output / 'ruler.json', report)
    print(json.dumps(report, ensure_ascii=False, indent=2))
    return 1 if report['status'] == 'FAIL' else 2


if __name__ == '__main__':
    try:
        _verify_cli_imports()
        code = main()
    except Exception as error:
        print(json.dumps({'status': 'BLOCKED', 'error': str(error)}, ensure_ascii=False))
        code = 1
    raise SystemExit(code)
