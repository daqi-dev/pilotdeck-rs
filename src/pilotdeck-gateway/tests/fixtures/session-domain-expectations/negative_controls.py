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

#1190: every canonical-output require is covered -- frames, durable statuses,
registry events / state / result, and the inapplicable-registry shape.  The
inapplicable require sits behind the structure applicability clause and the
registry.applicable scope surface (defense in depth), so its bound control
relaxes exactly those two earlier rejections inside its own run; deleting the
require must then let that corruption through.

Every mutant is compiled before it runs.  All selected rows run to
completion; any non-KILLED row is a nonzero exit.  A stale anchor or a failed
compile is INVALID and keeps going.  ``mutating_sources`` refuses files that
differ from HEAD, so this runner needs a real A5.2a ``--source`` and is meant
for the post-commit host run; an uncommitted tree is INVALID, never SURVIVED.

The case counts are never pinned in prose: they are derived from the panel's
``EXPECTATION_CASES`` at run time, written to ``derived-counts.json`` in the
evidence directory, and the binding tables below are machine-checked against
that same table, so a renamed, reruled or unbound control fails the run
instead of silently drifting the documented count.  fix-r1 (#1190 review
P2-1, escalated P1): ``exercised`` is built from ``BOUND`` alone (a control
that only ever appears as another row's unaffected witness proves nothing
about its own require), every ``BOUND`` entry must be nonempty, and
``BOUND``/``UNAFFECTED``/``MUTATIONS`` must name exactly the same mutation
set -- a deleted or renamed mutation row, an emptied or malformed binding
entry is ``BINDING DRIFT`` (exit 2) before any row executes, never a later
``IndexError`` that voids the whole table.

#1190 fix-r6: documentation does not maintain numeric copies. ``--list``
prints the current panel names, mutation names and derived counts without
creating evidence or executing controls. There is no prose parser, document
scan, frozen-directory adapter or document meta-control entry point.

Per-row results live in memory only.  ``summary.json`` is published exactly
once, after ``mutating_sources`` restored every guarded file and verified it
byte-for-byte against the baseline (#979/#1037 + #166 r8 semantics, shared
with #209 session-domain-lock-release): a guard or restoration failure voids
every already-recorded row and publishes INVALID instead of a false green
summary, and the payload is written to a sibling temp file and renamed.
"""
import argparse
import ast
import json
from pathlib import Path
import sys

sys.dont_write_bytecode = True
sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_process import mutating_sources, new_output_directory, run_owned
from negative_suite import invalidate_completed, publish_summary

ROOT = Path(__file__).resolve().parents[5]
SESSION = ROOT / 'scripts/gateway-parity/session_domain.py'
COMMON = ROOT / 'scripts/gateway-parity/common.py'
PANEL = ROOT / 'scripts/gateway-parity/controls/session_domain.py'
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
    # The frames require (the post-prefix enqueue order) is deleted (#1190).
    ('drop-frames-require', SESSION,
     "        require(tuple(row['rust']['frames']) == canonical['frames'], 'EXPECTATION_RUST_DERIVATION',\n"
     "                case + ': rust frames differ from the D4-B canonical output')\n",
     ""),
    # The durable-statuses require is deleted (#1190).
    ('drop-durable-status-require', SESSION,
     "        require(tuple(row['rust']['durableStatus']) == canonical['durableStatus'],\n"
     "                'EXPECTATION_RUST_DERIVATION',\n"
     "                case + ': rust durable statuses differ from the D4-B canonical output')\n",
     ""),
    # The inapplicable-registry require is neutralized: the branch body must
    # stay a statement, so the require becomes ``pass`` (#1190).
    ('drop-registry-inapplicable-require', SESSION,
     "            require(registry == {'applicable': False}, 'EXPECTATION_RUST_DERIVATION',\n"
     "                    case + ': rust registry must be inapplicable without a run registry')\n",
     "            pass\n"),
]

# Each mutation binds the controls whose corrupted fixture only the deleted
# require can reject, plus one control that must stay killed under the same
# mutant (a different require owns it).  The inapplicable require's bound
# control relaxes the structure applicability clause and the registry.applicable
# scope surface inside its own run (defense in depth: the canonical require
# is what must reject when the earlier layers are gone).
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
    'drop-frames-require': (
        ('expectations-rust-surface', 'EXPECTATION_RUST_DERIVATION'),
        ('expectations-rust-frame-swap', 'EXPECTATION_RUST_DERIVATION'),
    ),
    'drop-durable-status-require': (
        ('expectations-rust-no-registry-durable', 'EXPECTATION_RUST_DERIVATION'),
        ('expectations-rust-no-registry-status-durable', 'EXPECTATION_RUST_DERIVATION'),
    ),
    'drop-registry-inapplicable-require': (
        ('expectations-rust-registry-inapplicable', 'EXPECTATION_RUST_DERIVATION'),
    ),
}
UNAFFECTED = {
    'drop-registry-events-require': ('expectations-rust-frame-swap',
                                     'EXPECTATION_RUST_DERIVATION'),
    'drop-registry-state-require': ('expectations-rust-registry-events',
                                    'EXPECTATION_RUST_DERIVATION'),
    'drop-registry-result-require': ('expectations-rust-registry-state',
                                      'EXPECTATION_RUST_DERIVATION'),
    'drop-frames-require': ('expectations-rust-registry-state',
                            'EXPECTATION_RUST_DERIVATION'),
    'drop-durable-status-require': ('expectations-rust-frame-swap',
                                    'EXPECTATION_RUST_DERIVATION'),
    'drop-registry-inapplicable-require': ('expectations-rust-no-registry-durable',
                                            'EXPECTATION_RUST_DERIVATION'),
}


def panel_table(name):
    """One module-level str->str table of the controls panel, parsed not run.

    ``run_control.py`` loads that file from disk, so the same on-disk bytes are
    the authority here; ``ast.literal_eval`` never executes the apparatus
    bootstrap (same shape as ``committed_status_contract`` in
    ``negative_process``).
    """
    tree = ast.parse(PANEL.read_text(encoding='utf-8'), filename=str(PANEL))
    for node in tree.body:
        if (isinstance(node, ast.Assign) and len(node.targets) == 1
                and isinstance(node.targets[0], ast.Name)
                and node.targets[0].id == name):
            table = ast.literal_eval(node.value)
            if (not isinstance(table, dict) or not table
                    or not all(isinstance(key, str) and isinstance(value, str)
                               for key, value in table.items())):
                raise ValueError(f'{name} is not a nonempty str->str dict')
            return table
    raise ValueError(f'no module-level {name} assignment found')


def check_bindings(expectation_cases, mutations, bound, unaffected):
    """The binding tables must agree with the panel's registered controls.

    #1190 count drift: every bound/unaffected entry names a registered control
    with the rule the tables claim.  fix-r1 (review P2-1, escalated P1):
    ``exercised`` is built from ``BOUND`` alone -- a control that only ever
    appears as another row's unaffected witness is not covered -- every
    ``BOUND`` entry is nonempty, every entry is a (case, rule) pair of
    nonempty strings, and ``BOUND``/``UNAFFECTED``/``MUTATIONS`` name exactly
    the same mutation set.  A deleted or renamed mutation row, an emptied or
    malformed binding is reported here (exit 2) instead of passing the gate
    or crashing the row loop later.  fix-r2 (r2 review P2-1, P3-2): a
    malformed ``MUTATIONS`` row, a duplicated mutation name and a ``BOUND``
    value that is not a sequence of pairs are the same clean ``BINDING
    DRIFT`` -- the entry unpacking used to raise a traceback before any
    drift message could be printed.
    """
    errors = []
    names = set()
    for row in mutations:
        if (not isinstance(row, tuple) or len(row) != 4
                or not isinstance(row[0], str) or not row[0]):
            errors.append(f'MUTATIONS row is not a (name, source, old, new) '
                          f'row with a nonempty name: {row!r}')
            continue
        if row[0] in names:
            errors.append(f'MUTATIONS names are not unique: {row[0]}')
        names.add(row[0])
    for label, table in (('BOUND', bound), ('UNAFFECTED', unaffected)):
        for extra in sorted(set(table) - names):
            errors.append(f'{label} names unknown mutation {extra}')
        for missing in sorted(names - set(table)):
            errors.append(f'{label} has no entry for mutation {missing}')
    bound_exercised = set()
    for name in sorted(names & set(bound) & set(unaffected)):
        if isinstance(bound[name], (list, tuple)):
            entries = list(bound[name])
            if not entries:
                errors.append(f'BOUND[{name}] is empty')
        else:
            errors.append(f'BOUND[{name}] is not a sequence of (case, rule) '
                          f'pairs: {bound[name]!r}')
            entries = []
        for table, entry in ([('BOUND', entry) for entry in entries]
                             + [('UNAFFECTED', unaffected[name])]):
            if (not isinstance(entry, tuple) or len(entry) != 2
                    or not all(isinstance(part, str) and part for part in entry)):
                errors.append(f'{name}: {table} entry is not a (case, rule) '
                              f'pair of nonempty strings: {entry!r}')
                continue
            case, rule = entry
            registered = expectation_cases.get(case)
            if registered != rule:
                errors.append(f'{name}: {case} is claimed as {rule} '
                              f'but registered as {registered}')
            if table == 'BOUND':
                bound_exercised.add(case)
    missing = sorted(case for case, rule in expectation_cases.items()
                     if rule == 'EXPECTATION_RUST_DERIVATION' and case not in bound_exercised)
    if missing:
        errors.append('derivation controls not exercised by any BOUND mutation: '
                     + ', '.join(missing))
    return errors


def derived_counts(expectation_cases, calibration_cases, mutations):
    return {'expectationCases': len(expectation_cases),
            'derivationExpectationCases':
                sum(rule == 'EXPECTATION_RUST_DERIVATION'
                    for rule in expectation_cases.values()),
            'calibrationCases': len(calibration_cases),
            'mutationRows': len(mutations)}


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
    parser = argparse.ArgumentParser(description=__doc__, allow_abbrev=False)
    parser.add_argument('--output', type=Path)
    parser.add_argument('--source', type=Path,
                        help='fresh A5.2a calibration output (six-cells raw data)')
    parser.add_argument('--only', action='append')
    parser.add_argument('--check-anchors', action='store_true')
    parser.add_argument('--list', action='store_true',
                        help='print current control/mutation names and counts; read-only')
    parser.add_argument('--baseline', default='HEAD',
                        help='tree-ish the guarded sources must equal; HEAD for '
                             'the delivered post-commit run (a dirty rehearsal may '
                             'pass git stash create)')
    args = parser.parse_args()
    expectation_cases = panel_table('EXPECTATION_CASES')
    calibration_cases = panel_table('CALIBRATION_CASES')
    counts = derived_counts(expectation_cases, calibration_cases, MUTATIONS)
    drift = [('BINDING DRIFT', message) for message in
             check_bindings(expectation_cases, MUTATIONS, BOUND, UNAFFECTED)]
    if drift:
        for prefix, message in drift:
            print(prefix + ': ' + message, file=sys.stderr)
        return 2
    if not args.list:
        print('derived counts: ' + json.dumps(counts, sort_keys=True), flush=True)
    known = {row[0] for row in MUTATIONS}
    if args.only and (set(args.only) - known or len(args.only) != len(set(args.only))):
        print('unknown or duplicate --only name', file=sys.stderr)
        return 2
    selected = [row for row in MUTATIONS if not args.only or row[0] in args.only]
    if args.check_anchors:
        anchor_status = check_anchors(selected)
        if anchor_status != 0 or not args.list:
            return anchor_status
    if args.list:
        print(json.dumps({'EXPECTATION_CASES': expectation_cases,
                          'CALIBRATION_CASES': calibration_cases,
                          'MUTATIONS': [row[0] for row in MUTATIONS],
                          'counts': counts}, indent=2, ensure_ascii=False))
        return 0
    if args.output is None or args.source is None:
        print('--output and --source are required unless --list or --check-anchors is used',
              file=sys.stderr)
        return 2
    out = new_output_directory(args.output)
    source = args.source.resolve()
    (out / 'derived-counts.json').write_text(json.dumps(counts, sort_keys=True))
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
                # #1190: initialize before the guarded window. A stale anchor
                # or a failed compile used to raise before this binding, so
                # the row carried either no evidence (NameError voiding the
                # whole table) or the previous row's evidence.
                evidence = {'baseline': [], 'mutant': [], 'unaffected': None,
                            'restored': None}
                try:
                    original = originals[source_path].decode()
                    if original.count(old) != 1:
                        raise ValueError(f'anchor expired: count={original.count(old)}')
                    mutant = original.replace(old, new, 1)
                    compile(mutant, str(source_path), 'exec')
                    (out / (name + '-mutant.py')).write_text(mutant)
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
                        evidence['restored'] = detail
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
