#!/usr/bin/env python3
"""#1190 fix-r1 meta controls: the drift gates of negative_controls.py are
load-bearing.

The runner refuses to execute with exit 2 when its own binding tables or the
registration prose disagree with the derived counts.  This control mutates
exactly those assets in place (guarded by ``mutating_sources``, byte-verified
restoration) and requires the gate ``negative_controls.py --check-anchors`` to
refuse each corruption with its own drift message before any mutation row
could run:

* ``delete-state-mutation-row`` -- the #1190 review's exact scenario: one
  canonical-require mutation row disappears from ``MUTATIONS``.  The old
  ``check_bindings`` still passed (the dropped control stayed "exercised" as
  another row's unaffected witness); it must now be ``BINDING DRIFT``.
* ``empty-state-bound`` -- the same control's ``BOUND`` entry is emptied.
  The old runner crashed at ``bound[0]`` and voided the whole table; it must
  now be ``BINDING DRIFT``.
* ``readme-stale-count`` -- the fixture README pins a wrong count again; the
  doc scan must refuse it as ``COUNT DRIFT``.

Every mutant is compiled before it runs.  All selected rows run to
completion; each records KILLED / SURVIVED / INVALID with its reason and the
summary is published exactly once, after ``mutating_sources`` restored and
re-verified every guarded byte (#979/#1037 semantics, shared with
``negative_controls.py``).  Any non-KILLED row is a nonzero exit.
``mutating_sources`` refuses files that differ from ``--baseline`` (default
HEAD), so the delivered run is the post-commit host run; a dirty rehearsal
may pass ``--baseline "$(git stash create)"``.
"""
import argparse
import json
from pathlib import Path
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_process import mutating_sources, new_output_directory, run_owned
from negative_suite import invalidate_completed, publish_summary

ROOT = Path(__file__).resolve().parents[5]
NEGCTL = Path(__file__).resolve().with_name('negative_controls.py')
README = Path(__file__).resolve().with_name('README.md')
GATE = [sys.executable, '-I', str(NEGCTL), '--check-anchors']

MUTATIONS = [
    # The registry-state mutation row is deleted from MUTATIONS outright.
    ('delete-state-mutation-row', NEGCTL,
     "    ('drop-registry-state-require', SESSION,\n"
     "     \"            require(registry['state'] == canonical['state'], 'EXPECTATION_RUST_DERIVATION',\\n\"\n"
     "     \"                    case + ': rust registry state differs from the D4-B canonical output')\\n\",\n"
     "     \"\"),\n",
     ""),
    # The same row's BOUND entry is emptied; the gate must refuse, not crash.
    ('empty-state-bound', NEGCTL,
     "    'drop-registry-state-require': (\n"
     "        ('expectations-rust-registry-state', 'EXPECTATION_RUST_DERIVATION'),\n"
     "    ),\n",
     "    'drop-registry-state-require': (),\n"),
    # The README pins a count the derived tables no longer have.
    ('readme-stale-count', README,
     "里的 canonical `require`，清单以该表为准",
     "里的全部七条 canonical `require`"),
]

# Each mutant is bound to the exact drift message that names it; a bare
# non-zero exit or an unrelated traceback is INVALID, never a kill.
MARKERS = {
    'delete-state-mutation-row':
        'BOUND names unknown mutation drop-registry-state-require',
    'empty-state-bound':
        'BOUND[drop-registry-state-require] is empty',
    'readme-stale-count':
        "'七条' pins 7 but derived mutationRows is 6",
}


def run_gate(out, label):
    result = run_owned(GATE, cwd=ROOT, timeout=300)
    (out / (label + '.log')).write_text(result.stdout + result.stderr)
    return result


def gate_ok(result):
    return result.returncode == 0 and 'anchors: ok' in result.stdout


def gate_refused(result, marker):
    return (result.returncode == 2 and marker in result.stderr
            and 'Traceback' not in result.stderr)


def _counts(results):
    return {state: sum(row['status'] == state for row in results)
            for state in ('KILLED', 'SURVIVED', 'INVALID')}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--only', action='append')
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
    out = new_output_directory(args.output)
    results = []
    try:
        paths = [NEGCTL, README]
        with mutating_sources(ROOT, paths, baseline=args.baseline) as originals:
            for name, source, old, new in selected:
                row = {'mutation': name, 'status': 'INVALID', 'reason': 'not executed'}
                evidence = {'positive': None, 'mutant': None, 'restored': None}
                try:
                    original = originals[source].decode()
                    if original.count(old) != 1:
                        raise ValueError(f'anchor expired: count={original.count(old)}')
                    mutant = original.replace(old, new, 1)
                    if source.suffix == '.py':
                        compile(mutant, str(source), 'exec')
                    positive = run_gate(out, f'{name}-positive')
                    if not gate_ok(positive):
                        raise ValueError('positive gate did not pass: '
                                        + positive.stdout.strip()[-200:])
                    evidence['positive'] = 'gate passed unmutated'
                    source.write_text(mutant)
                    killed = run_gate(out, f'{name}-mutant')
                    if gate_refused(killed, MARKERS[name]):
                        evidence['mutant'] = MARKERS[name]
                        row['status'], row['reason'] = 'KILLED', \
                            'gate refused with: ' + MARKERS[name]
                    elif killed.returncode == 0:
                        row['reason'] = 'mutated asset still passed the gate'
                    else:
                        row['reason'] = (f'unexpected gate exit {killed.returncode}'
                                         + (f' ({killed.stderr.strip()[-200:]})'
                                            if killed.stderr.strip() else ''))
                except Exception as error:
                    row['status'], row['reason'] = 'INVALID', f'{type(error).__name__}: {error}'
                finally:
                    source.write_bytes(originals[source])
                if row['status'] == 'KILLED':
                    restored = run_gate(out, f'{name}-restored')
                    if gate_ok(restored):
                        evidence['restored'] = 'gate passed again after restore'
                        row['reason'] += '; restored: gate green'
                    else:
                        row['status'], row['reason'] = 'INVALID', \
                            'restored gate failed: ' + restored.stdout.strip()[-200:]
                row['evidence'] = evidence
                results.append(row)
                print(f"{name}: {row['status']}: {row['reason']}", flush=True)
    except BaseException as error:
        reason = f'source guard: {type(error).__name__}: {error}'
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
