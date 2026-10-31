#!/usr/bin/env python3
"""#1140 native deadline wiring: the new requires are load-bearing.

Mutates the frozen gateway apparatus in place (``scripts/gateway-parity/
session_domain.py``) one #1140 addition at a time and requires the bound
controls (``run_control.py``) to change their verdict exactly:

* baseline (unmutated): the bound control FAILs with its registered rule;
* mutant: the corruption the deleted/bypassed line used to reject must now
  change the control's verdict.  Most mutations let the corrupted fixture
  through, so the control itself must report that slippage --
  FAIL/CONTROL_EXPECTATION "mutation accepted".  Three mutations are caught
  differently and the table pins each expected verdict precisely:
  the binding-source require deletion lets ``forged-binding-source`` pass
  outright (exit 0); the batch-aggregation bypass is caught by every native
  control's healthy precondition (the batch must stay NOT_RUN); the hardcoded
  native cell status is caught by ``native-surface-mismatch``'s own
  assertion; and the manifest-equality deletion is caught by the
  pre-existing ``audit_native`` COVERAGE require with the audit's detail --
  the fast provenance line is defense-in-depth, deleting it must not open a
  hole, and a mutant that reaches "mutation accepted" is a failure;
* unaffected: a control bound to a different require must still be killed
  under the same mutant (skipped for the batch bypass: the healthy
  precondition of every native control fires first, which is itself the
  bound observation);
* restored: after the bytes are put back, the bound control fails again.

Every mutant is compiled before it runs.  All selected rows run to
completion; any non-KILLED row is a nonzero exit.  A stale anchor or a failed
compile is INVALID and keeps going.  ``mutating_sources`` refuses files that
differ from HEAD, so this runner is meant for the post-commit host run; an
uncommitted tree is INVALID, never SURVIVED.

Per-row results live in memory only.  ``summary.json`` is published exactly
once, after ``mutating_sources`` restored every guarded file and verified it
byte-for-byte against the baseline (#979/#1037 + #166 r8 semantics, shared
with #209 session-domain-lock-release and #1177 session-domain-expectations):
a guard or restoration failure voids every already-recorded row and publishes
INVALID instead of a false green summary, and the payload is written to a
sibling temp file and renamed.
"""
import argparse
import json
import os
from pathlib import Path
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_process import mutating_sources, new_output_directory, run_owned
from negative_suite import invalidate_completed, publish_summary

ROOT = Path(__file__).resolve().parents[5]
SESSION = ROOT / 'scripts/gateway-parity/session_domain.py'
COMMON = ROOT / 'scripts/gateway-parity/common.py'
RUN_CONTROL = Path(__file__).resolve().with_name('run_control.py')

MUTATIONS = [
    # The loader's provenance rule dies: a binding pre-registered before
    # launch is accepted.
    ('accept-preregistered-binding', SESSION,
     "    for key in ('binding', 'observerRows', 'bindingSource'):\n"
     "        require(key not in launch and key not in prelaunch, 'BINDING',\n"
     "                'runtime binding/observer records must not be pre-registered before launch')\n",
     "    pass\n"),
    # The driver-ledger/egress-witness cross-check dies: egress the strace
    # witness lost is no longer noticed even though the provider ledger
    # still shows the request.
    ('drop-egress-crosscheck', SESSION,
     "    require(not requests or provider['endpoint'] in targets, 'NETWORK_WITNESS',\n"
     "            'driver-observed provider traffic missing from the egress witness')\n",
     "    pass\n"),
    # audit_native's #1140 provenance require dies: a forged binding source
    # audits like an observer-recorded one.
    ('drop-binding-source-require', SESSION,
     "    require(contract.get('bindingSource') == 'observer-hello', 'BINDING',\n"
     "            'binding must be recorded by the independent observer at hello and bound '\n"
     "            'to the contract afterwards; pre-registered/SYNTHETIC bindings are rejected')\n",
     "    pass\n"),
    # The loader's fail-fast manifest-equality require dies; the audit's
    # pre-existing COVERAGE shadow must still reject a shrunk contract.
    ('drop-manifest-equality', SESSION,
     "    require(contract.get('cases') == expected, 'COVERAGE',\n"
     "            'the native capture must declare the full E1 case manifest; no denominator shrink')\n",
     "    pass\n"),
    # The batch aggregation bypass: NOT_RUN cells are dropped from the one
    # shared aggregation, so a native PASS would make the batch PASS.
    ('bypass-batch-aggregation', SESSION,
     "    statuses = [cell['status'] for cell in cells]\n",
     "    statuses = [cell['status'] for cell in cells if cell['status'] != 'NOT_RUN']\n"),
    # The native cell status is hardcoded instead of aggregated from the
    # per-case verdicts.
    ('hardcode-native-cell-pass', SESSION,
     "            cell['status'], _ = common.aggregate_status(*(row['status'] for row in native['verdicts']))\n",
     "            cell['status'] = 'PASS'\n"),
]

# Each mutation binds the control whose corrupted capture only the mutated
# line rejects, with that control's registered rule for the baseline and
# restored runs.
BOUND = {
    'accept-preregistered-binding': ('native-preregistered-binding', 'BINDING'),
    'drop-egress-crosscheck': ('native-missing-egress', 'NETWORK_WITNESS'),
    'drop-binding-source-require': ('forged-binding-source', 'BINDING'),
    'drop-manifest-equality': ('native-shrunk-manifest', 'COVERAGE'),
    'bypass-batch-aggregation': ('native-missing-descendant', 'PROCESS_WITNESS'),
    'hardcode-native-cell-pass': ('native-surface-mismatch', 'EXPECTATION_MISMATCH'),
}

# What the bound control must report under the mutant.  ``shape`` selects the
# classifier: 'accepted' FAILs with CONTROL_EXPECTATION "mutation accepted";
# 'accepted-pass' passes outright; 'precondition' FAILs with the control's
# healthy precondition (the batch must stay NOT_RUN); 'assertion' FAILs with
# the named control assertion; 'shadow' FAILs with the audit's COVERAGE rule
# and the audit's own detail (the loader's fast line is dead but the
# pre-existing audit require still closes the hole).
MUTANT = {
    'accept-preregistered-binding': {'shape': 'accepted'},
    'drop-egress-crosscheck': {'shape': 'accepted'},
    'drop-binding-source-require': {'shape': 'accepted-pass'},
    'drop-manifest-equality': {'shape': 'shadow',
                               'rule': 'COVERAGE',
                               'detail': 'missing/duplicate/extra native groups'},
    'bypass-batch-aggregation': {'shape': 'precondition',
                                 'rule': 'CONTROL_PRECONDITION'},
    'hardcode-native-cell-pass': {'shape': 'assertion',
                                  'rule': 'CONTROL_EXPECTATION',
                                  'detail': 'a single mutated surface must fail exactly one case on that surface'},
}

# One control bound to a different require must stay killed under the same
# mutant.  The batch bypass has no entry: every native control's healthy
# precondition fires first, which is the bound observation itself.
UNAFFECTED = {
    'accept-preregistered-binding': ('native-missing-descendant', 'PROCESS_WITNESS'),
    'drop-egress-crosscheck': ('native-missing-descendant', 'PROCESS_WITNESS'),
    'drop-binding-source-require': ('native-missing-descendant', 'PROCESS_WITNESS'),
    'drop-manifest-equality': ('native-missing-descendant', 'PROCESS_WITNESS'),
    'hardcode-native-cell-pass': ('native-missing-descendant', 'PROCESS_WITNESS'),
}


def run_control(out, label, name):
    """One control run in a fresh interpreter; returns its parsed verdict."""
    case_dir = out / (label + '.dir')
    result = run_owned([sys.executable, '-I', str(RUN_CONTROL), name,
                        str(case_dir)], cwd=ROOT, timeout=300)
    (out / (label + '.log')).write_text(result.stdout + result.stderr)
    verdict = None
    for line in reversed(result.stdout.splitlines()):
        try:
            verdict = json.loads(line)
            break
        except ValueError:
            continue
    return result.returncode, verdict


def classify_bound(code, verdict, case, expected_rule):
    """Baseline/restored/unaffected shape: the control must FAIL with its rule."""
    if verdict is None or verdict.get('case') != case:
        return f'no verdict JSON (exit {code})'
    if code == 1 and verdict.get('status') == 'FAIL' and verdict.get('rule') == expected_rule:
        return None
    return f'expected FAIL/{expected_rule}, got exit {code} {verdict}'


def classify_mutant(code, verdict, case, baseline_rule):
    """Classify the bound control's verdict under the mutant.

    The expected mutant shape is checked first because the 'shadow' shape
    fails with the same rule as the baseline (the audit's COVERAGE, not the
    deleted loader line); only a verdict that matches neither is a survivor.
    """
    ok, why = mutant_ok(code, verdict, case)
    if ok:
        return 'ok', why
    if verdict is None or verdict.get('case') != BOUND[case][0]:
        return 'invalid', f'no verdict JSON (exit {code})'
    failed = verdict.get('status') == 'FAIL'
    rule, detail = verdict.get('rule'), verdict.get('detail', '')
    if code == 1 and failed and rule == baseline_rule and 'mutation accepted' not in detail:
        return 'survived', 'bound control still failed with ' + baseline_rule
    return 'invalid', f'unexpected mutant verdict: exit {code} {verdict}'


def mutant_ok(code, verdict, case):
    """Return (ok, detail) for the mutant-phase verdict against the table."""
    spec = MUTANT[case]
    if verdict is None or verdict.get('case') != BOUND[case][0]:
        return False, f'no verdict JSON (exit {code})'
    failed = verdict.get('status') == 'FAIL'
    rule, detail = verdict.get('rule'), verdict.get('detail', '')
    shape = spec['shape']
    if shape == 'accepted':
        ok = code == 1 and failed and rule == 'CONTROL_EXPECTATION' \
            and 'mutation accepted' in detail
        return ok, 'control reported its mutation accepted by the mutant'
    if shape == 'accepted-pass':
        ok = code == 0 and verdict.get('status') == 'PASS'
        return ok, 'control accepted the corrupted contract'
    if shape == 'precondition':
        ok = code == 1 and failed and rule == spec['rule']
        return ok, 'control precondition caught the bypass: ' + detail
    if shape == 'assertion':
        ok = code == 1 and failed and rule == spec['rule'] and spec['detail'] in detail
        return ok, 'control assertion caught the bypass: ' + detail
    if shape == 'shadow':
        ok = code == 1 and failed and rule == spec['rule'] and spec['detail'] in detail
        return ok, 'audit COVERAGE shadow rejected the shrunk contract: ' + detail
    return False, 'unknown mutant shape ' + shape


def check_anchors(selected):
    for name, source, old, _new in selected:
        count = source.read_text().count(old)
        if count != 1:
            print(f'STALE ANCHOR {name}: {count} matches in {source.relative_to(ROOT)}')
            return 2
    print(f'{len(selected)} anchors: ok')
    return 0


def _counts(results):
    return {state: sum(row['status'] == state for row in results)
            for state in ('KILLED', 'SURVIVED', 'INVALID')}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output', type=Path)
    parser.add_argument('--only', action='append')
    parser.add_argument('--check-anchors', action='store_true')
    parser.add_argument('--baseline', default='HEAD',
                        help='tree-ish the guarded sources must equal; HEAD for '
                             'the delivered post-commit run (a dirty rehearsal may '
                             'pass git stash create)')
    args = parser.parse_args()
    known = {row[0] for row in MUTATIONS}
    if args.only and (set(args.only) - known or len(args.only) != len(set(args.only))):
        print('unknown or duplicate --only name', file=sys.stderr)
        return 2
    selected = [row for row in MUTATIONS if not args.only or row[0] in args.only]
    if args.check_anchors:
        return check_anchors(selected)
    if args.output is None:
        print('--output is required unless --check-anchors is used', file=sys.stderr)
        return 2
    if not os.environ.get('CARGO_TARGET_DIR'):
        # Never create or default a second build directory (#741 rule).
        raise SystemExit('CARGO_TARGET_DIR must be supplied by the caller (never create a second target)')
    out = new_output_directory(args.output)
    results = []
    try:
        # One lease only: mutating_sources acquires the real parity lease
        # through common.parity_lock, and gateway-parity paths require
        # common.py in the guarded set even when --only selects SESSION.
        paths = [SESSION, COMMON]
        with mutating_sources(ROOT, paths, baseline=args.baseline) as originals:
            for name, source_path, old, new in selected:
                row = {'mutation': name, 'status': 'INVALID', 'reason': 'not executed'}
                bound_case, bound_rule = BOUND[name]
                unaffected = UNAFFECTED.get(name)
                try:
                    original = originals[source_path].decode()
                    if original.count(old) != 1:
                        raise ValueError(f'anchor expired: count={original.count(old)}')
                    mutant = original.replace(old, new, 1)
                    compile(mutant, str(source_path), 'exec')
                    (out / (name + '-mutant.py')).write_text(mutant)
                    evidence = {'baseline': None, 'mutant': None, 'unaffected': None,
                                'restored': None}
                    code, verdict = run_control(out, f'{name}-baseline', bound_case)
                    problem = classify_bound(code, verdict, bound_case, bound_rule)
                    if problem is not None:
                        raise ValueError(f'baseline: {problem}')
                    evidence['baseline'] = f'failed with {bound_rule}'
                    source_path.write_text(mutant)
                    code, verdict = run_control(out, f'{name}-mutant', bound_case)
                    outcome, detail = classify_mutant(code, verdict, name, bound_rule)
                    if outcome == 'survived':
                        row['status'], row['reason'] = 'SURVIVED', detail
                    elif outcome == 'invalid':
                        row['status'], row['reason'] = 'INVALID', detail
                    else:
                        ok, why = mutant_ok(code, verdict, name, bound_rule)
                        if not ok:
                            row['status'], row['reason'] = 'INVALID', \
                                f'unexpected mutant verdict: exit {code} {verdict}'
                        else:
                            evidence['mutant'] = why
                            if unaffected is None:
                                row['status'] = 'KILLED'
                                row['reason'] = why + ' (batch bypass: every native ' \
                                                'control precondition fires first)'
                            else:
                                code, verdict = run_control(out, f'{name}-unaffected',
                                                             unaffected[0])
                                problem = classify_bound(code, verdict, unaffected[0],
                                                         unaffected[1])
                                if problem is not None:
                                    row['status'], row['reason'] = 'INVALID', \
                                        'unaffected control also disabled: ' + problem
                                else:
                                    evidence['unaffected'] = \
                                        f'still failed with {unaffected[1]}'
                                    row['status'] = 'KILLED'
                                    row['reason'] = why
                except Exception as error:
                    row['status'], row['reason'] = 'INVALID', f'{type(error).__name__}: {error}'
                finally:
                    source_path.write_bytes(originals[source_path])
                if row['status'] == 'KILLED':
                    code, verdict = run_control(out, f'{name}-restored', bound_case)
                    problem = classify_bound(code, verdict, bound_case, bound_rule)
                    if problem is None:
                        row['reason'] += '; restored: ' + evidence['baseline']
                        evidence['restored'] = evidence['baseline']
                    else:
                        row['status'], row['reason'] = 'INVALID', \
                            'restored baseline not re-killed: ' + problem
                row['evidence'] = evidence
                results.append(row)
                print(f"{name}: {row['status']}: {row['reason']}", flush=True)
    except BaseException as error:
        reason = f'source guard: {type(error).__name__}: {error}'
        # A guard/restoration failure voids every already-recorded row, then
        # publishes INVALID (#979/#1037); a missing summary stays fail-closed.
        invalidate_completed(results, reason)
        for name, *_rest in selected[len(results):]:
            results.append({'mutation': name, 'status': 'INVALID', 'reason': reason})
        publish_summary(out, results)
        if not isinstance(error, Exception):
            raise
        return 2
    publish_summary(out, results)
    counts = _counts(results)
    print(json.dumps(counts), flush=True)
    return 0 if counts['KILLED'] == len(results) else 1


if __name__ == '__main__':
    raise SystemExit(main())
