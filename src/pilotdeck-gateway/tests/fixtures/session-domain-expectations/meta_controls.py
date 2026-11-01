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
* fix-r2 (r2 review P2-2): ``unscanned-freeze-event`` writes a new event file
  with a stale count into a throwaway copy of ``frozen/`` (fix-r4) -- the
  doc-scan set is derived from that directory's glob, so the gate (run with
  ``--frozen-dir <copy>``) must refuse the file's stale count.
* fix-r3 (r3 review P1): ``delete-known-freeze-event`` /
  ``rename-known-freeze-event`` delete a committed #1190 event, or rename it
  to a name the doc-scan globs do not match, inside the copy; the gate's
  scan-set floor (``check_scan_set``) must refuse that as its own ``COUNT
  DRIFT``.

fix-r4 (r4 review P0/P1/P2, coordinator: change the design, not another
patch): every fixture scenario above runs on a ``shutil.copytree`` copy of the
repository's ``specs/gateway-parity/frozen/`` inside a
``tempfile.TemporaryDirectory()`` -- the repository directory is never
created in, renamed in or deleted from, the copy's cleanup belongs to the
context manager (a killed process can only leave the copy under /tmp), and
the ownership bookkeeping of fix-r3 (exclusive creation, the ``owned`` ledger
and the post-window fallback removal) is deleted instead of patched again.
A rename still refuses any existing directory entry at its target --
``os.path.lexists``, unlike ``Path.exists()``, sees a dangling symlink
(r4 review P0).  The event names this table uses are pinned against the
gate's own ``KNOWN_EVENTS`` floor (single source of truth, r4 review opus
row 4), and a tripwire snapshots the repository ``frozen/`` (symlinks and
dangling links captured by their target string) before and after the run: a
run that changed it is voided wholesale.  ``--self-test`` reruns the three
filesystem rows as a subprocess (all KILLED, repository ``frozen/``
byte-for-byte unchanged), proves the snapshot really detects a removed or
retargeted dangling symlink, and proves the occupied-target refusal.

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
import fnmatch
import json
import os
from pathlib import Path
import shutil
import sys
import tempfile

sys.path.insert(0, str(Path(__file__).resolve().parent))
sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_controls import KNOWN_EVENTS
from negative_process import mutating_sources, new_output_directory, run_owned
from negative_suite import invalidate_completed, publish_summary

ROOT = Path(__file__).resolve().parents[5]
NEGCTL = Path(__file__).resolve().with_name('negative_controls.py')
README = Path(__file__).resolve().with_name('README.md')
FROZEN = ROOT / 'specs/gateway-parity/frozen'
# The doc-scan globs the gate derives its event set from (fix-r4): the
# fixture name must be matched by them, and the rename target must not, so
# each scenario reaches the exact drift message it is bound to.
SCAN_GLOBS = ('*-1190.md', '*-1190-*.md')
FIXTURE = '20261101-1190-r99.md'
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

# Create-only fixture (r2 review P2-2, fix-r4 redesign): a new event file is
# written into a throwaway copy of ``frozen/``; the gate must refuse it there
# and the repository directory is never touched.  ``old`` is None to mark a
# row that is not a text replacement.
CREATED = [
    ('unscanned-freeze-event',
     FIXTURE,
     None,
     '# 负控夹具（unscanned-freeze-event，绝不提交；落在 frozen/ 通配内必须被拒）\n'
     '本事件钉死了七条 canonical require。\n'),
]

# Filesystem rows (fix-r3, r3 review P1; fix-r4 redesign): a known event is
# deleted, or renamed to a name the doc-scan globs do not match, inside the
# copy; the gate must refuse that as its own COUNT DRIFT (a shrunk scan set
# used to read as "no drift").  The names are pinned against the gate's own
# KNOWN_EVENTS floor, so this table cannot drift from the single source of
# truth (r4 review opus row 4).
FS_MUTATIONS = [
    ('delete-known-freeze-event', '20261101-1190.md', None),
    ('rename-known-freeze-event', '20261101-1190-r2.md', '20261101-1190r2.md'),
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


def run_gate(out, label, frozen_dir=None):
    command = GATE if frozen_dir is None else GATE + ['--frozen-dir', str(frozen_dir)]
    result = run_owned(command, cwd=ROOT, timeout=300)
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


def snapshot_dir(directory):
    """fix-r4: symlink-aware byte snapshot of a directory's direct entries.

    A symlink is captured by its target string -- a dangling one included,
    which ``Path.exists()`` reports as absent and ``read_bytes()`` would fail
    on -- so the tripwire can assert the repository ``frozen/`` is
    byte-for-byte unchanged including links.
    """
    state = {}
    for path in sorted(directory.iterdir()):
        if path.is_symlink():
            state[path.name] = ('symlink', os.readlink(path))
        elif path.is_file():
            state[path.name] = ('file', path.read_bytes())
        else:
            state[path.name] = ('other',)
    return state


def frozen_drift(before):
    """Drift of the repository ``frozen/`` against ``before`` (fix-r4)."""
    after = snapshot_dir(FROZEN)
    return [f'{name}: {before.get(name)!r} -> {after.get(name)!r}'
            for name in sorted(set(before) | set(after))
            if before.get(name) != after.get(name)]


def scenario_copy(window):
    """A throwaway copy of the repository ``frozen/`` (fix-r4 redesign).

    Every fixture scenario runs on this copy, so the repository directory is
    never created in, renamed in or deleted from; the ``TemporaryDirectory``
    owns the cleanup and a killed process can only leave the copy under /tmp.
    """
    return shutil.copytree(FROZEN, Path(window) / 'frozen', symlinks=True)


def apply_fs_mutation(copy_dir, event_name, rename_to):
    """Apply one filesystem scenario to the throwaway copy.

    fix-r4 (r4 review P0): a rename refuses any existing directory entry at
    the target -- ``os.path.lexists`` sees a dangling symlink that
    ``Path.exists()`` reports as absent -- so the move can never overwrite a
    pre-existing entry, even if the copy ever pointed back at real data.
    """
    source = copy_dir / event_name
    if not source.is_file():
        raise ValueError(f'known event is missing from the copy: {event_name}')
    if rename_to is None:
        source.unlink()
        return None
    target = copy_dir / rename_to
    if os.path.lexists(target):
        raise ValueError(f'rename target already exists: {rename_to}')
    return source.rename(target)


def _selftest_real_frozen_unchanged(out, baseline):
    """fix-r4 main regression: the filesystem rows leave the repo alone.

    The three filesystem rows really run (as a subprocess, every one of them
    KILLED) and the repository ``frozen/`` -- including any symlink or
    dangling link -- is byte-for-byte unchanged afterwards.
    """
    before = snapshot_dir(FROZEN)
    result = run_owned(
        [sys.executable, '-I', str(Path(__file__).resolve()), '--output',
         str(out / 'real-frozen-unchanged.dir'), '--only', 'unscanned-freeze-event',
         '--only', 'delete-known-freeze-event', '--only', 'rename-known-freeze-event',
         '--baseline', baseline],
        cwd=ROOT, timeout=300)
    (out / 'real-frozen-unchanged.log').write_text(result.stdout + result.stderr)
    summary = json.loads((out / 'real-frozen-unchanged.dir' / 'summary.json').read_text())
    rows = summary.get('controls', [])
    if result.returncode != 0 or len(rows) != 3 \
            or any(row.get('status') != 'KILLED' for row in rows):
        raise ValueError(f'inner run: exit {result.returncode}, {len(rows)} rows: '
                         + json.dumps(rows, ensure_ascii=False))
    drift = frozen_drift(before)
    if drift:
        raise ValueError('repository frozen/ changed: ' + '; '.join(drift))


def _selftest_snapshot_drift():
    """fix-r4: the tripwire's snapshot really captures links.

    An unmodified directory compares equal, and each of a removed dangling
    symlink, a retargeted dangling symlink, a retargeted regular symlink,
    changed file bytes and an added file compares different.
    """
    with tempfile.TemporaryDirectory() as window:

        def fresh_lab():
            lab = Path(window) / 'lab'
            shutil.rmtree(lab, ignore_errors=True)
            lab.mkdir()
            (lab / 'event.md').write_text('body\n')
            (lab / 'alias.md').symlink_to('event.md')
            (lab / 'dangling.md').symlink_to('nowhere.md')
            return lab

        lab = fresh_lab()
        before = snapshot_dir(lab)
        if snapshot_dir(lab) != before:
            raise ValueError('an unmodified directory compared different')

        def drifts_when(message, mutate):
            lab = fresh_lab()
            before = snapshot_dir(lab)
            mutate(lab)
            if snapshot_dir(lab) == before:
                raise ValueError(message + ' compared equal')

        def remove_dangling(lab):
            (lab / 'dangling.md').unlink()

        def retarget_dangling(lab):
            (lab / 'dangling.md').unlink()
            (lab / 'dangling.md').symlink_to('elsewhere.md')

        def retarget_alias(lab):
            (lab / 'alias.md').unlink()
            (lab / 'alias.md').symlink_to('nowhere.md')

        def change_file(lab):
            (lab / 'event.md').write_text('changed\n')

        def add_file(lab):
            (lab / 'extra.md').write_text('added\n')

        for message, mutate in (
                ('a removed dangling symlink', remove_dangling),
                ('a retargeted dangling symlink', retarget_dangling),
                ('a retargeted regular symlink', retarget_alias),
                ('changed file bytes', change_file),
                ('an added file', add_file)):
            drifts_when(message, mutate)


def _selftest_rename_refusal():
    """fix-r4 (r4 review P0): an occupied rename target is refused, kept.

    A rename whose target is a dangling symlink or a regular file is refused
    with the row's own message; the source file and both occupied entries
    keep their bytes.
    """
    with tempfile.TemporaryDirectory() as window:
        lab = Path(window) / 'lab'
        lab.mkdir()
        (lab / '20261101-1190-r2.md').write_text('event bytes\n')
        (lab / 'dangling.md').symlink_to('nowhere.md')
        (lab / 'occupied.md').write_text('occupied bytes\n')
        for target in ('dangling.md', 'occupied.md'):
            try:
                apply_fs_mutation(lab, '20261101-1190-r2.md', target)
            except ValueError as error:
                if 'rename target already exists' not in str(error):
                    raise
            else:
                raise ValueError(f'rename onto {target} was accepted')
            if (lab / '20261101-1190-r2.md').read_text() != 'event bytes\n':
                raise ValueError('the source file was modified')
        if os.readlink(lab / 'dangling.md') != 'nowhere.md':
            raise ValueError('the dangling symlink was replaced')
        if (lab / 'occupied.md').read_text() != 'occupied bytes\n':
            raise ValueError('the occupied file was replaced')


def run_self_test(out, baseline):
    """fix-r4: the lifecycle regressions of the filesystem scenarios."""
    scenarios = (
        ('real-frozen-unchanged', _selftest_real_frozen_unchanged, (out, baseline)),
        ('snapshot-detects-drift', _selftest_snapshot_drift, ()),
        ('rename-refuses-occupied-target', _selftest_rename_refusal, ()),
    )
    results = []
    for name, scenario, arguments in scenarios:
        row = {'scenario': name, 'status': 'INVALID', 'reason': 'not executed'}
        try:
            scenario(*arguments)
            row['status'], row['reason'] = 'PASS', 'regression held'
        except Exception as error:
            row['reason'] = f'{type(error).__name__}: {error}'
        results.append(row)
        print(f"{name}: {row['status']}: {row['reason']}", flush=True)
    publish_summary(out, results)
    return 0 if all(row['status'] == 'PASS' for row in results) else 1


def _check_tables():
    """Pin the scenario tables to the gate's own floor (fix-r4, opus row 4)."""
    for _name, event_name, rename_to in FS_MUTATIONS:
        if event_name not in KNOWN_EVENTS:
            return f'FS_MUTATIONS names an event outside KNOWN_EVENTS: {event_name}'
        if rename_to is not None and (
                '1190' not in rename_to
                or any(fnmatch.fnmatch(rename_to, pattern) for pattern in SCAN_GLOBS)):
            return f'rename target escapes the reverse scan coverage: {rename_to}'
    if FIXTURE in KNOWN_EVENTS or not any(
            fnmatch.fnmatch(FIXTURE, pattern) for pattern in SCAN_GLOBS):
        return f'fixture name is not a scanned-glob match outside KNOWN_EVENTS: {FIXTURE}'
    return None


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--only', action='append')
    parser.add_argument('--self-test', action='store_true',
                        help='run the fix-r4 lifecycle regressions of the '
                             'filesystem rows instead of the mutation rows')
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
    table_error = _check_tables()
    if table_error is not None:
        print(table_error, file=sys.stderr)
        return 2
    text_selected = [row for row in MUTATIONS if not args.only or row[0] in args.only]
    created_selected = [row for row in CREATED if not args.only or row[0] in args.only]
    fs_selected = [row for row in FS_MUTATIONS if not args.only or row[0] in args.only]
    ordered_names = [row[0] for row in [*text_selected, *created_selected, *fs_selected]]
    results = []
    # fix-r4 tripwire: the repository frozen/ must be byte-for-byte unchanged
    # (including symlinks and dangling links) by the time this run is over.
    before = snapshot_dir(FROZEN)
    try:
        paths = [NEGCTL, README]
        with mutating_sources(ROOT, paths, baseline=args.baseline) as originals:
            for name, source, old, new in text_selected:
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
                                        + (positive.stderr.strip()[-200:]
                                           or positive.stdout.strip()[-200:]))
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
            for name, fixture, _old, content in created_selected:
                row = {'mutation': name, 'status': 'INVALID', 'reason': 'not executed'}
                evidence = {'positive': None, 'mutant': None, 'restored': None}
                try:
                    positive = run_gate(out, f'{name}-positive')
                    if not gate_ok(positive):
                        raise ValueError('positive gate did not pass: '
                                        + (positive.stderr.strip()[-200:]
                                           or positive.stdout.strip()[-200:]))
                    evidence['positive'] = 'gate passed unmutated on the repository directory'
                    # fix-r4: the fixture is created inside the throwaway
                    # copy only; the repository frozen/ is not touched.
                    with tempfile.TemporaryDirectory(prefix='meta-controls-frozen-') as window:
                        copy_dir = scenario_copy(window)
                        (copy_dir / fixture).write_text(content, encoding='utf-8')
                        killed = run_gate(out, f'{name}-mutant', copy_dir)
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
            for name, event_name, rename_to in fs_selected:
                row = {'mutation': name, 'status': 'INVALID', 'reason': 'not executed'}
                evidence = {'positive': None, 'mutant': None, 'restored': None}
                try:
                    positive = run_gate(out, f'{name}-positive')
                    if not gate_ok(positive):
                        raise ValueError('positive gate did not pass: '
                                        + (positive.stderr.strip()[-200:]
                                           or positive.stdout.strip()[-200:]))
                    evidence['positive'] = 'gate passed unmutated on the repository directory'
                    # fix-r4: the scenario runs inside the throwaway copy
                    # only; the repository frozen/ is not touched.
                    with tempfile.TemporaryDirectory(prefix='meta-controls-frozen-') as window:
                        copy_dir = scenario_copy(window)
                        apply_fs_mutation(copy_dir, event_name, rename_to)
                        killed = run_gate(out, f'{name}-mutant', copy_dir)
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
        for name in ordered_names[len(results):]:
            results.append({'mutation': name, 'status': 'INVALID', 'reason': reason})
        publish_summary(out, results)
        drift = frozen_drift(before)
        if drift:
            print('real frozen directory changed: ' + '; '.join(drift), file=sys.stderr)
        if not isinstance(error, Exception):
            raise
        return 2
    drift = frozen_drift(before)
    if drift:
        invalidate_completed(results, 'real frozen directory changed: '
                            + '; '.join(drift))
        publish_summary(out, results)
        print('real frozen directory changed: ' + '; '.join(drift), file=sys.stderr)
        return 2
    publish_summary(out, results)
    counts = _counts(results)
    print(json.dumps(counts), flush=True)
    return 0 if counts['KILLED'] == len(results) else 1


if __name__ == '__main__':
    raise SystemExit(main())
