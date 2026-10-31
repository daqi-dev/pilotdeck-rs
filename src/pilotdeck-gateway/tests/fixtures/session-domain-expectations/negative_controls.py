#!/usr/bin/env python3
"""#1177 fix-r1 negative controls: the canonical-output requires are load-bearing.

Mutates the frozen gateway apparatus in place (``scripts/gateway-parity/
session_domain.py``) by deleting one ``validate_expectations`` require at a
time and requires the bound A5.4 expectation control (``run_control.py``) to
change its verdict exactly as the #1177 review demanded:

* baseline (unmutated): the named control must FAIL with its expected rule;
* mutant (require deleted): the corrupted fixture is accepted by the mutated
  validator, and the control must report exactly that -- FAIL /
  CONTROL_EXPECTATION with "mutation accepted".  Still failing with the same
  expected rule is SURVIVED, because the control was never bound to the
  deleted require;
* unaffected: one control bound to a different require must still be killed
  under the same mutant, so a deleted require cannot silently disable the
  whole derivation check;
* restored: after the bytes are put back, the bound control fails again.

Every mutant is compiled before it runs.  All selected rows run to
completion; any non-KILLED row is a nonzero exit.  A stale anchor or a failed
compile is INVALID and keeps going.  ``mutating_sources`` refuses files that
differ from HEAD, so this runner needs a real A5.2a ``--source`` and is meant
for the post-commit host run; an uncommitted tree is INVALID, never SURVIVED.

Per-row results live in memory only.  ``summary.json`` is published exactly
once, after ``mutating_sources`` restored every guarded file and verified it
byte-for-byte against the baseline (#979/#1037 + #166 r8 semantics, shared
with #209 session-domain-lock-release): a guard or restoration failure voids
every already-recorded row and publishes INVALID instead of a false green
summary, and the payload is written to a sibling temp file and renamed.
"""
import argparse
import json
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
    # The registry-events canonical-output require (D2's no-S(T) assertion and
    # the anchored event sequence) is deleted outright.
    ('drop-registry-events-require', SESSION,
     "            require(registry['applicable'] is True and registry['events'] == expected_events,\n"
     "                    'EXPECTATION_RUST_DERIVATION',\n"
     "                    case + ': rust registry events differ from the D4-B canonical output')\n",
     ""),
    # The registry-state require (D3's weakest derivation: running) is deleted.
    ('drop-registry-state-require', SESSION,
     "            require(registry['state'] == canonical['state'], 'EXPECTATION_RUST_DERIVATION',\n"
     "                    case + ': rust registry state differs from the D4-B canonical output')\n",
     ""),
    # The registry-result require (D3's absent result) is deleted.
    ('drop-registry-result-require', SESSION,
     "            require(registry['result'] == canonical['result'], 'EXPECTATION_RUST_DERIVATION',\n"
     "                    case + ': rust registry result differs from the D4-B canonical output')\n",
     ""),
]

# Each mutation binds the controls whose corrupted fixture only the deleted
# require can reject, plus one control that must stay killed under the same
# mutant (a different require owns it).
BOUND = {
    'drop-registry-events-require': (
        ('expectations-rust-registry-events', 'EXPECTATION_RUST_DERIVATION'),
        ('expectations-rust-events-timeout-status', 'EXPECTATION_RUST_DERIVATION'),
    ),
    'drop-registry-state-require': (
        ('expectations-rust-registry-state', 'EXPECTATION_RUST_DERIVATION'),
    ),
    'drop-registry-result-require': (
        ('expectations-rust-registry-result', 'EXPECTATION_RUST_DERIVATION'),
    ),
}
UNAFFECTED = {
    'drop-registry-events-require': ('expectations-rust-frame-swap',
                                     'EXPECTATION_RUST_DERIVATION'),
    'drop-registry-state-require': ('expectations-rust-registry-events',
                                    'EXPECTATION_RUST_DERIVATION'),
    'drop-registry-result-require': ('expectations-rust-registry-state',
                                      'EXPECTATION_RUST_DERIVATION'),
}


def run_control(out, label, name, source):
    """One control run in a fresh interpreter; returns its parsed verdict."""
    case_dir = out / (label + '.dir')
    result = run_owned([sys.executable, '-I', str(RUN_CONTROL), name,
                        str(source), str(case_dir)], cwd=ROOT, timeout=300)
    (out / (label + '.log')).write_text(result.stdout + result.stderr)
    verdict = None
    for line in reversed(result.stdout.splitlines()):
        try:
            verdict = json.loads(line)
            break
        except ValueError:
            continue
    return result.returncode, verdict


def run_bound(out, label, name, expected_rule, source, expect_fail):
    """Run one control and classify it against the expected outcome.

    ``expect_fail`` is True for the baseline/restored/unaffected runs (the
    control must FAIL with ``expected_rule``) and False for the mutant run.
    Under the mutant the corrupted fixture is accepted by the mutated
    validator, so the control itself must report that its mutation slipped
    through: FAIL / CONTROL_EXPECTATION with "mutation accepted".  A control
    that still FAILs with ``expected_rule`` means the deleted require was not
    the one catching it (SURVIVED).  Returns (outcome, detail) with outcome
    one of ok / survived / invalid.
    """
    code, verdict = run_control(out, label, name, source)
    if verdict is None or verdict.get('case') != name:
        return 'invalid', f'no verdict JSON (exit {code})'
    failed = verdict.get('status') == 'FAIL'
    if expect_fail:
        if code == 1 and failed and verdict.get('rule') == expected_rule:
            return 'ok', 'failed with ' + expected_rule
        return 'invalid', f"expected FAIL/{expected_rule}, got exit {code} {verdict}"
    if code == 1 and failed and verdict.get('rule') == 'CONTROL_EXPECTATION' \
            and 'mutation accepted' in verdict.get('detail', ''):
        return 'ok', 'control reported its mutation accepted by the mutant'
    if code == 1 and failed and verdict.get('rule') == expected_rule:
        return 'survived', 'bound control still failed with ' + expected_rule
    return 'invalid', f'unexpected mutant verdict: exit {code} {verdict}'


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
    parser.add_argument('--source', type=Path,
                        help='fresh A5.2a calibration output (six-cells raw data)')
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
    if args.output is None or args.source is None:
        print('--output and --source are required unless --check-anchors is used',
              file=sys.stderr)
        return 2
    out = new_output_directory(args.output)
    source = args.source.resolve()
    results = []
    try:
        # One lease only: mutating_sources acquires the real parity lease
        # through common.parity_lock, and gateway-parity paths require
        # common.py in the guarded set even when --only selects SESSION.
        paths = [SESSION, COMMON]
        with mutating_sources(ROOT, paths, baseline=args.baseline) as originals:
            for name, source_path, old, new in selected:
                row = {'mutation': name, 'status': 'INVALID', 'reason': 'not executed'}
                bound = BOUND[name]
                unaffected = UNAFFECTED[name]
                try:
                    original = originals[source_path].decode()
                    if original.count(old) != 1:
                        raise ValueError(f'anchor expired: count={original.count(old)}')
                    mutant = original.replace(old, new, 1)
                    compile(mutant, str(source_path), 'exec')
                    (out / (name + '-mutant.py')).write_text(mutant)
                    evidence = {'baseline': [], 'mutant': [], 'unaffected': None,
                                'restored': None}
                    for case, rule in bound:
                        outcome, detail = run_bound(out, f'{name}-baseline-{case}', case,
                                                    rule, source, expect_fail=True)
                        if outcome != 'ok':
                            raise ValueError(f'baseline {case}: {detail}')
                        evidence['baseline'].append(detail)
                    source_path.write_text(mutant)
                    for case, rule in bound:
                        outcome, detail = run_bound(out, f'{name}-mutant-{case}', case,
                                                    rule, source, expect_fail=False)
                        if outcome != 'ok':
                            row['status'] = outcome.upper()
                            row['reason'] = detail
                            break
                        evidence['mutant'].append(detail)
                    else:
                        case, rule = unaffected
                        outcome, detail = run_bound(out, f'{name}-unaffected-{case}', case,
                                                    rule, source, expect_fail=True)
                        if outcome != 'ok':
                            row['status'], row['reason'] = 'INVALID', \
                                'unaffected control also disabled: ' + detail
                        else:
                            evidence['unaffected'] = detail
                            row['status'] = 'KILLED'
                            row['reason'] = 'bound control reported its mutation ' \
                                            'accepted by the mutant'
                except Exception as error:
                    row['status'], row['reason'] = 'INVALID', f'{type(error).__name__}: {error}'
                finally:
                    source_path.write_bytes(originals[source_path])
                if row['status'] == 'KILLED':
                    case, rule = bound[0]
                    outcome, detail = run_bound(out, f'{name}-restored-{case}', case,
                                                rule, source, expect_fail=True)
                    if outcome == 'ok':
                        row['reason'] += '; restored: ' + detail
                    else:
                        row['status'], row['reason'] = 'INVALID', \
                            'restored baseline not re-killed: ' + detail
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
