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
* fix-r2 (r2 review P2-1): ``malformed-state-bound`` (a 1-tuple entry),
  ``non-sequence-state-bound`` (a non-sequence ``BOUND`` value) and
  ``malformed-state-mutation-row`` (a 3-tuple ``MUTATIONS`` row) used to
  raise an unpacking traceback; the gate must report each as its own
  ``BINDING DRIFT`` instead.
* fix-r2 (r2 review P3-2): ``duplicate-state-mutation-row`` repeats one
  ``MUTATIONS`` row; the gate must refuse the duplicated name.
* fix-r2 (r2 review P2-4): ``readme-stale-count-ge`` pins the count with the
  个 unit the old ``COUNT_PHRASE`` could not read; the scan must refuse it.
* fix-r2 (r2 review P2-2): ``unscanned-freeze-event`` writes a new event
  file into ``specs/gateway-parity/frozen/`` -- the doc-scan set is derived
  from that directory's glob, so the gate must refuse the file's stale
  count and pass again once it is removed.  The file is a fixture created
  and deleted inside this run (never a tracked product source; ``specs/``
  is outside the tree guard and outside the anchor grammar), so it is not a
  text-replacement row and lives in ``CREATED`` with ``old`` None.
* fix-r3 (r3 review P0, Codex + opus concurrency independently): a
  ``CREATED`` fixture is taken through exclusive creation
  (``open(source, 'x', encoding='utf-8')``) and a ``created`` flag; the
  row's ``finally`` removes only a file this run actually created, so a
  pre-existing fixture is refused with ``ValueError`` and its bytes are
  kept.  A last-resort pass after the guarded window
  (``_remove_owned``) removes a leftover fixture only while it still
  holds the exact bytes this run wrote, so the path can never be reused to
  delete someone else's file.  ``--self-test`` reruns the row as a
  subprocess with the fixture path occupied (bytes must survive) and with
  the positive gate already refusing (the fixture must never be created).
* fix-r3 (r3 review P1): ``delete-known-freeze-event`` /
  ``rename-known-freeze-event`` remove a committed #1190 event or rename it
  to a name the doc-scan globs do not match; the gate's scan-set floor
  (``check_scan_set``) must refuse that as its own ``COUNT DRIFT``.  The
  event files are tracked, so they ride in the ``mutating_sources`` guard
  set (snapshot restore + byte re-verification on window exit) and each row
  restores its own bytes before the next row's positive gate.

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
# The committed #1190 event files (fix-r3): they join the guarded set so a
# filesystem row can restore them byte-for-byte, and the gate's scan-set
# floor pins them by name.
EVENTS = (ROOT / 'specs/gateway-parity/frozen/20261101-1190.md',
          ROOT / 'specs/gateway-parity/frozen/20261101-1190-r2.md')
FIXTURE = ROOT / 'specs/gateway-parity/frozen/20261101-1190-r99.md'
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
    # r2 review P2-1: the BOUND entry loses a column; the gate must refuse
    # the malformed (case,) shape instead of unpacking it.
    ('malformed-state-bound', NEGCTL,
     "    'drop-registry-state-require': (\n"
     "        ('expectations-rust-registry-state', 'EXPECTATION_RUST_DERIVATION'),\n"
     "    ),\n",
     "    'drop-registry-state-require': (\n"
     "        ('expectations-rust-registry-state',),\n"
     "    ),\n"),
    # r2 review P2-1: the whole BOUND value stops being a sequence; the gate
    # must report it, not crash on ``list(BOUND[name])``.
    ('non-sequence-state-bound', NEGCTL,
     "    'drop-registry-state-require': (\n"
     "        ('expectations-rust-registry-state', 'EXPECTATION_RUST_DERIVATION'),\n"
     "    ),\n",
     "    'drop-registry-state-require': 42,\n"),
    # r2 review P2-1 (sibling of the BOUND entry): the MUTATIONS row loses
    # its replacement column; the gate must refuse the 3-tuple shape.
    ('malformed-state-mutation-row', NEGCTL,
     "     \"                    case + ': rust registry state differs from the D4-B canonical output')\\n\",\n"
     "     \"\"),\n",
     "     \"                    case + ': rust registry state differs from the D4-B canonical output')\\n\"),\n"),
    # r2 review P3-2: the same MUTATIONS row is added twice; a duplicated
    # name must not silently pass with a larger mutationRows count.
    ('duplicate-state-mutation-row', NEGCTL,
     "    ('drop-registry-state-require', SESSION,\n"
     "     \"            require(registry['state'] == canonical['state'], 'EXPECTATION_RUST_DERIVATION',\\n\"\n"
     "     \"                    case + ': rust registry state differs from the D4-B canonical output')\\n\",\n"
     "     \"\"),\n",
     "    ('drop-registry-state-require', SESSION,\n"
     "     \"            require(registry['state'] == canonical['state'], 'EXPECTATION_RUST_DERIVATION',\\n\"\n"
     "     \"                    case + ': rust registry state differs from the D4-B canonical output')\\n\",\n"
     "     \"\"),\n"
     "    ('drop-registry-state-require', SESSION,\n"
     "     \"            require(registry['state'] == canonical['state'], 'EXPECTATION_RUST_DERIVATION',\\n\"\n"
     "     \"                    case + ': rust registry state differs from the D4-B canonical output')\\n\",\n"
     "     \"\"),\n"),
    # r2 review P2-4: the same stale count pinned with the 个 unit.
    ('readme-stale-count-ge', README,
     "里的 canonical `require`，清单以该表为准",
     "里的全部 7 个 canonical `require`"),
]

# Create-only fixtures (r2 review P2-2): the file must not exist yet, the
# gate must refuse it, and the fixture is deleted afterwards.  ``old`` is None
# to mark a row that is not a text replacement.  fix-r3 (r3 review P0): the
# file is created exclusively (``open(..., 'x')``), so a pre-existing file is
# refused and its bytes are kept instead of being unlinked by ``finally``.
CREATED = [
    ('unscanned-freeze-event',
     FIXTURE,
     None,
     '# 负控夹具（unscanned-freeze-event，绝不提交；落在 frozen/ 通配内必须被拒）\n'
     '本事件钉死了七条 canonical require。\n'),
]

# Filesystem rows (fix-r3, r3 review P1): a committed #1190 event is deleted,
# or renamed to a name the doc-scan globs do not match; the gate's scan-set
# floor must refuse that as its own COUNT DRIFT (a shrunk scan set used to
# read as "no drift").  (name, source, rename target or None for deletion.)
# These are not text replacements, and the sources are tracked files: each row
# restores its own bytes in ``finally`` from the guard snapshot, and the
# ``mutating_sources`` window exit re-verifies them against the baseline.
FS_MUTATIONS = [
    ('delete-known-freeze-event', EVENTS[0], None),
    ('rename-known-freeze-event', EVENTS[1], '20261101-1190r2.md'),
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
    'malformed-state-bound':
        "drop-registry-state-require: BOUND entry is not a (case, rule) "
        "pair of nonempty strings: ('expectations-rust-registry-state',)",
    'non-sequence-state-bound':
        'BOUND[drop-registry-state-require] is not a sequence of '
        '(case, rule) pairs: 42',
    'malformed-state-mutation-row':
        'MUTATIONS row is not a (name, source, old, new) row with a nonempty '
        "name: ('drop-registry-state-require',",
    'duplicate-state-mutation-row':
        'MUTATIONS names are not unique: drop-registry-state-require',
    'readme-stale-count-ge':
        "'7 个' pins 7 but derived mutationRows is 6",
    'unscanned-freeze-event':
        "20261101-1190-r99.md:2: '七条' pins 7 but derived mutationRows is 6",
    'delete-known-freeze-event':
        'event file 20261101-1190.md is missing from the doc scan set',
    'rename-known-freeze-event':
        'frozen event 20261101-1190r2.md matches no doc scan glob',
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


def _remove_owned(owned):
    """fix-r3 (r3 review P2): last-resort removal of this run's fixtures.

    A row's own ``finally`` already removed what it created; this pass only
    catches a leftover after the row loop died on the way out (a RuntimeError
    raised by that ``finally``, a guard failure, a cancellation).  A file is
    removed only while it still holds the exact bytes this run wrote
    exclusively, so a path someone else re-used is never deleted.
    """
    for path, payload in owned:
        try:
            if path.read_bytes() == payload:
                path.unlink()
        except FileNotFoundError:
            continue


def run_self_test(out, baseline):
    """fix-r3 (r3 review P0): the CREATED row's lifecycle regressions.

    Each scenario plants a file this test owns (exclusive creation) and then
    runs the real ``--only unscanned-freeze-event`` row as a subprocess:

    * ``preexisting-fixture-bytes-kept``: the fixture path is occupied.  The
      row must be INVALID with ``fixture already exists`` and the planted
      bytes must survive the run unchanged.
    * ``positive-failure-no-create``: another #1190-shaped event file pins a
      stale count, so the positive gate refuses before the fixture is
      created; the fixture path must not exist afterwards.

    Returns 0 only when both hold.
    """
    scenarios = [
        ('preexisting-fixture-bytes-kept',
         FIXTURE,
         b'pre-existing bytes owned by the fix-r3 self-test\n',
         'fixture already exists'),
        ('positive-failure-no-create',
         ROOT / 'specs/gateway-parity/frozen/20261101-1190-r98.md',
         '# 负控夹具（fix-r3 self-test，绝不提交；让正控闸门先拒）\n'
         '本事件钉死了七条 canonical require。\n'.encode('utf-8'),
         'positive gate did not pass'),
    ]
    results = []
    for name, plant, payload, refuse in scenarios:
        row = {'scenario': name, 'status': 'INVALID', 'reason': 'not executed'}
        planted = False
        try:
            try:
                with open(plant, 'xb') as handle:
                    handle.write(payload)
                planted = True
            except FileExistsError as error:
                raise ValueError(f'self-test plant already exists: {plant}') from error
            result = run_owned(
                [sys.executable, '-I', str(Path(__file__).resolve()), '--output',
                 str(out / (name + '.dir')), '--only', 'unscanned-freeze-event',
                 '--baseline', baseline],
                cwd=ROOT, timeout=300)
            (out / (name + '.log')).write_text(result.stdout + result.stderr)
            summary = json.loads((out / (name + '.dir') / 'summary.json').read_text())
            rows = summary.get('controls', [])
            if result.returncode != 1 or len(rows) != 1:
                raise ValueError(f'unexpected inner run: exit {result.returncode}, '
                                 f'{len(rows)} rows')
            reason = rows[0].get('reason', '')
            if rows[0].get('status') != 'INVALID' or refuse not in reason:
                raise ValueError(f'row was not refused as {refuse!r}: {reason}')
            if name == 'preexisting-fixture-bytes-kept':
                if FIXTURE.read_bytes() != payload:
                    raise ValueError('pre-existing fixture bytes were modified')
            elif FIXTURE.exists():
                raise ValueError('fixture was created although the positive gate '
                                 'refused first')
            row['status'], row['reason'] = 'PASS', \
                'row refused as ' + refuse + ' and left foreign bytes alone'
        except Exception as error:
            row['reason'] = f'{type(error).__name__}: {error}'
        finally:
            if planted:
                try:
                    if plant.read_bytes() == payload:
                        plant.unlink()
                except FileNotFoundError:
                    pass
        results.append(row)
        print(f"{name}: {row['status']}: {row['reason']}", flush=True)
    publish_summary(out, results)
    return 0 if all(row['status'] == 'PASS' for row in results) else 1


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--only', action='append')
    parser.add_argument('--self-test', action='store_true',
                        help='run the fix-r3 lifecycle regressions of the '
                             'CREATED row instead of the mutation rows')
    parser.add_argument('--baseline', default='HEAD',
                        help='tree-ish the guarded sources must equal; HEAD for '
                             'the delivered post-commit run (a dirty rehearsal may '
                             'pass git stash create)')
    args = parser.parse_args()
    out = new_output_directory(args.output)
    if args.self_test:
        if args.only:
            print('--self-test cannot be combined with --only', file=sys.stderr)
            return 2
        return run_self_test(out, args.baseline)
    rows = [*MUTATIONS, *CREATED]
    known = {row[0] for row in rows} | {row[0] for row in FS_MUTATIONS}
    if args.only and (set(args.only) - known or len(args.only) != len(set(args.only))):
        print('unknown or duplicate --only name', file=sys.stderr)
        return 2
    selected = [row for row in rows if not args.only or row[0] in args.only]
    fs_selected = [row for row in FS_MUTATIONS if not args.only or row[0] in args.only]
    results = []
    owned = []
    try:
        try:
            paths = [NEGCTL, README, *EVENTS]
            with mutating_sources(ROOT, paths, baseline=args.baseline) as originals:
                for name, source, old, new in selected:
                    row = {'mutation': name, 'status': 'INVALID', 'reason': 'not executed'}
                    evidence = {'positive': None, 'mutant': None, 'restored': None}
                    created = False
                    try:
                        if old is None:
                            original = None
                        else:
                            original = originals[source].decode()
                            if original.count(old) != 1:
                                raise ValueError(f'anchor expired: count={original.count(old)}')
                        mutant = new if original is None else original.replace(old, new, 1)
                        if original is not None and source.suffix == '.py':
                            compile(mutant, str(source), 'exec')
                        positive = run_gate(out, f'{name}-positive')
                        if not gate_ok(positive):
                            raise ValueError('positive gate did not pass: '
                                            + (positive.stderr.strip()[-200:]
                                               or positive.stdout.strip()[-200:]))
                        evidence['positive'] = 'gate passed unmutated'
                        if original is None:
                            # fix-r3 (r3 review P0): exclusive creation proves
                            # this run owns the file; a pre-existing fixture is
                            # refused with its bytes kept, never unlinked.
                            try:
                                with open(source, 'x', encoding='utf-8') as handle:
                                    handle.write(mutant)
                            except FileExistsError as error:
                                raise ValueError(f'fixture already exists, keeping '
                                                 f'its bytes: {source}') from error
                            created = True
                            owned.append((source, mutant.encode('utf-8')))
                        else:
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
                        # fix-r3 (r3 review P0): unlink only what this run
                        # created; a pre-existing file keeps its bytes.
                        if old is None:
                            if created:
                                source.unlink(missing_ok=True)
                                if source.exists():
                                    raise RuntimeError(f'fixture was not removed: {source}')
                        else:
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
                for name, source, rename_to in fs_selected:
                    row = {'mutation': name, 'status': 'INVALID', 'reason': 'not executed'}
                    evidence = {'positive': None, 'mutant': None, 'restored': None}
                    original = None
                    moved = None
                    try:
                        original = originals[source]
                        positive = run_gate(out, f'{name}-positive')
                        if not gate_ok(positive):
                            raise ValueError('positive gate did not pass: '
                                            + (positive.stderr.strip()[-200:]
                                               or positive.stdout.strip()[-200:]))
                        evidence['positive'] = 'gate passed unmutated'
                        if rename_to is None:
                            source.unlink()
                        else:
                            target = source.with_name(rename_to)
                            if target.exists():
                                raise ValueError(f'rename target already exists: {target}')
                            moved = source.rename(target)
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
                        if moved is not None and moved.exists():
                            moved.rename(source)
                        if original is not None:
                            if not source.exists():
                                source.write_bytes(original)
                            if source.read_bytes() != original:
                                raise RuntimeError(f'event file was not restored: {source}')
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
        finally:
            _remove_owned(owned)
    except BaseException as error:
        reason = f'source guard: {type(error).__name__}: {error}'
        invalidate_completed(results, reason)
        for name, *_rest in [*selected, *fs_selected][len(results):]:
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
