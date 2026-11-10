#!/usr/bin/env python3
# anchor-selfcheck: unsupported meta-test of the roster verify-before-publish guard, drives the entry's main() with a stubbed cargo
"""#2309 fix-r3: the verify-before-publish roster check needs a denominator.

Two review rounds found that ``manager-gateway/negative_controls.py``
re-checks the recorded verdicts against the static ``EXPECTED_NAMES`` roster
*before* publishing anything that reads as success -- and that nothing
pinned that defence: deleting one ``EXPECTED_NAMES`` entry, or rewriting the
``recorded != expected`` predicate to a constant, keeps every existing gate
green (DS boundary / MM boundary, both families). The startup matrix in
``test_negative_optimize_guards.py`` proves the refusals *before* the run;
the end-of-run re-check, the ``roster-mismatch.json``-instead-of-summary
publication, the subset run's recorded selection, the atomic mutant/restore
writes, the terminate handlers of the self-held window, and the
pending-reanchor registry's startup placement had no machine check.

This file drives the real ``main()`` with only the executor layer stubbed
(``mutating_sources``' snapshot window, ``write_source_atomic``, the slot
gate, ``run_owned`` and the pass/kill judges); ``registered`` / ``select`` /
``claim_dir``, the real anchor ``mutate`` over the real source bytes, the
roster re-check and every publish go through the entry's real code. No
cargo, no slot, no tree mutation: the stubbed window hands out the real
bytes it would snapshot, and the stubbed atomic writer records writes
instead of touching the tree, so every file write lands in the temporary
evidence directory. The same design means a stale anchor in the cases table
turns the full-run case red here (the real ``mutate`` refuses), which is the
desired fail-closed direction.

The kill variants this is designed to catch, each demonstrated red/green in
the fix-r3 evidence: dropping the last ``EXPECTED_NAMES`` entry (the full-run
case stops exiting 0), replacing ``if recorded != expected:`` with
``if False:`` (the shrunken-run case sees a published summary and exit 0),
removing the ``selected`` key from the summary payload (the subset case can
no longer audit what ran), reintroducing a truncating ``write_text`` /
``write_bytes`` for the mutant or the restore (the atomic-write count drops
below two per case), dropping the ``install_terminate_handlers`` call (the
window's handler never appears), and moving the registry read back into the
guarded window (an unusable registry stops being a "never started" refusal).

fix-r3 (review round 2) adds two things. The drive now holds the same
``negative_process.tree_lock`` a real run would (with a test-sized 60 s
budget, not the production 900 s default): the stubbed window still hands
out live source bytes, so a concurrently mutating batch on this checkout
could feed the real ``mutate`` a mutant instead of the anchor and turn this
pin red for no defect. And the three sibling entries that carry the same
verify-before-publish defence -- ``memory-gateway``, ``snapshot-projection``
and ``sdk-mcp-session-tools`` -- get source-anchor pins (the behavioural
drive stays manager-gateway-only: memory-gateway and snapshot-projection
write their mutants with bare ``Path.write_text`` inside the window -- the
truncating in-place write whose SIGKILL half-file risk is why #1903
introduced ``write_source_atomic`` -- so a stubbed-window drive would still
touch the guarded tree; sdk-mcp-session-tools writes through the shared
``mutating_sources`` window and is behaviourally drivable, registered as the
first extension in #2804). Each anchor test applies two kill demos to a
doctored copy of its own source -- ``if recorded != expected:`` flipped to
``if False:``, and every anchor-bearing line commented out -- and requires
the anchors to break either way, so the pin cannot be satisfied by comment
lines while the real code is gone (the line filter's known blind spot is
documented at ``check_anchors``).

The lock import caveat (round 3, MM concurrency#4): ``tree_lock`` itself is
imported before the lock is taken, and ``negative_process.py`` is an in-place
mutation target for other lanes -- an import landing inside such a window
would cache a mutant in ``sys.modules`` and this drive would run it. No
known mutant today touches the flock, so the present hazard is the comment
overstating the guarantee rather than a wrong verdict; the hardening
(post-lock byte verification, or relocating the lock helper out of the
mutated module) is registered in #2804.

Run from this directory: ``python3 -m unittest test_negative_roster_guards``.
"""
import contextlib
import importlib.util
import io
import json
import os
from pathlib import Path
import shutil
import signal
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

import pending_reanchor as pending
from negative_process import tree_lock

FIXTURES = Path(__file__).resolve().parent
ENTRY = FIXTURES / 'manager-gateway' / 'negative_controls.py'
FULL_ROSTER = 36

# The sibling entries that carry the same verify-before-publish roster
# defence (review round 2, MiniMax sibling #1 P1): the ordered source
# anchors of that defence in each entry. The behavioural drive covers
# manager-gateway only -- memory-gateway and snapshot-projection write
# their mutants with bare ``Path.write_text`` inside the window, so
# driving their ``main()`` on a stubbed window would still write the
# guarded tree. The anchors pin the same kill demo the drive pins: a
# flipped ``if recorded != expected:`` predicate, a missing
# roster-mismatch publication, or a summary published before the
# refusal all break the ordered anchor chain.
#
# The sdk-mcp chain pins the derivation of ``expected`` with a two-part
# anchor (the comprehension head and its ``if`` tail span two lines there,
# so a one-fragment tail would let a constant table replace the derivation
# while the chain stays green -- round 3, MM sibling#4).
SIBLING_DEFENCE_ANCHORS = {
    'memory-gateway': (
        'EXPECTED_NAMES = (',
        'expected = [n for n in EXPECTED_NAMES if not selected or n in selected]',
        'if recorded != expected:',
        '(out / "roster-mismatch.json").write_text(',
        'return 1',
        '(out / "summary.json").write_text(',
    ),
    'snapshot-projection': (
        'EXPECTED_NAMES = (',
        'expected = [n for n in EXPECTED_NAMES if not selected or n in selected]',
        'if recorded != expected:',
        '(out / "roster-mismatch.json").write_text(',
        'return 1',
        '(out / "summary.json").write_text(',
    ),
    'sdk-mcp-session-tools': (
        'EXPECTED_NAMES = (',
        'expected = [name for name in EXPECTED_NAMES',
        'if not args.selected or name in args.selected]',
        'if recorded != expected:',
        "(output / 'roster-mismatch.json').write_text(",
        'return 1',
        "'summary.json.tmp'",
    ),
}

# The declared sibling denominator (#2773 fix-r3, review MM r3 boundary#1):
# the anchor table's keys double as the loop domain of both tests below, so
# they need a static literal of their own -- deleting one key would silently
# shorten the anchor test and the kill demo while the table itself stays
# green. Every loop iterates this tuple, never the table.
EXPECTED_SIBLINGS = ('memory-gateway', 'snapshot-projection',
                     'sdk-mcp-session-tools')
TERMINATE_SIGNALS = ('SIGTERM', 'SIGHUP', 'SIGQUIT')


def load_entry():
    """Import the real entry under test (the source file is the truth, never a copy)."""
    spec = importlib.util.spec_from_file_location('manager_gateway_negative_controls',
                                                  ENTRY)
    module = importlib.util.module_from_spec(spec)
    saved = sys.path[:]
    try:
        spec.loader.exec_module(module)
    finally:
        # The entry prepends its import roots at module level; keep the test
        # interpreter's path as it was.
        sys.path[:] = saved
    return module


@contextlib.contextmanager
def stubbed_window(root, paths, *, baseline=None, lock_timeout=None):
    """The real snapshot window refuses a dirty tree by design; hand out bytes.

    The test tree carries the fix under review, so the pristine-vs-HEAD check
    would refuse every run. The bytes are the real current sources -- the
    real ``mutate`` below still walks every real anchor.
    """
    yield {path: path.read_bytes() for path in paths}


@contextlib.contextmanager
def held_tree_lock(root):
    """The same lock a real run holds, on a test-sized budget.

    The production default is a 900 s silent wait; a mutating batch holding
    this checkout would turn this unit test into a 15-minute hang followed by
    an opaque ``TimeoutError`` (#2773 fix-r3, review MM r3 concurrency#3 /
    boundary#5). Six drives each pass through this gate, so the wait would
    stack. 60 s names the cause instead.
    """
    # Only the acquisition is rewritten (#2773 fix-r4, review DS r4
    # boundary#2 / concurrency#2; r5 concurrency#1 / boundary#1 / simplify#1):
    # wrapping the whole ``with`` would swallow a TimeoutError raised by the
    # drive body itself, the acquisition must sit inside the finally-protected
    # region so an async exception in the enter-to-finally window cannot leak
    # the held lock at statement boundaries (a sub-instruction hit inside
    # ``enter_context`` before the exit callback registers stays below this
    # guarantee; #2773 fix-r6, review DS r6 concurrency#1), and ExitStack
    # keeps the with-protocol exception forwarding for the body (a bare
    # ``lock.__exit__(None, None, None)`` would always claim "no exception").
    stack = contextlib.ExitStack()
    try:
        try:
            stack.enter_context(tree_lock(root, timeout=60))
        except TimeoutError as error:
            raise AssertionError(
                '#2773 fix-r3 the roster-guard drive could not take the tree '
                'lock within 60 s -- a mutating batch holds this checkout; '
                f'this is not an entry defect: {error}') from error
        with stack:
            yield
    finally:
        stack.close()


class RecordingRegistry:
    """Delegate to the real registry, recording the identity ``for_script`` got."""

    def __init__(self, real):
        self._real = real
        self.calls = []

    def for_script(self, root, script=None):
        self.calls.append(script)
        return self._real.for_script(root, script)

    def __getattr__(self, name):
        return getattr(self._real, name)


class UnusableRegistry:
    """The registry shape ``pending.for_script`` raises on for a bad identity."""

    @staticmethod
    def for_script(root, script=None):
        raise ValueError('pending-reanchor requires a fixture script identity')


class ManagerRosterGuard(unittest.TestCase):
    """The roster re-check must gate every summary that reads as success."""

    def drive(self, argv, *, drop_last=False, shrink_roster=False, registry=None):
        """Run the real ``main()``; return (module, code, out, stdout, stderr, writes)."""
        module = load_entry()
        base = Path(tempfile.mkdtemp(prefix='negctl-2309-'))
        self.addCleanup(shutil.rmtree, base, ignore_errors=True)
        out = base / 'out'
        writes = []
        real_select = module.select

        def dropping_select(controls, requested, **kwargs):
            rows = real_select(controls, requested, **kwargs)
            # The injected shrink: a select() that silently loses the last row
            # is the #1666 hazard this guard exists to catch.
            return rows[:-1] if drop_last and rows else rows

        def fake_write_atomic(root, path, data, *, mode=None):
            writes.append((Path(path), bytes(data)))

        def fake_slot(script, **kwargs):
            return subprocess.CompletedProcess([], 0, '', '')

        def fake_run_owned(command, **kwargs):
            test = command[-4]
            return subprocess.CompletedProcess(
                command, 0,
                f'test {test} ... ok\ntest result: ok. 1 passed; 0 failed;\n', '')

        # main() installs its terminate handlers on this very process; the
        # test interpreter must keep its own dispositions afterwards.
        saved_handlers = {name: signal.getsignal(getattr(signal, name))
                          for name in TERMINATE_SIGNALS if hasattr(signal, name)}
        installed = False
        stdout, stderr = io.StringIO(), io.StringIO()
        # A launcher identity that is NOT the entry path (a wrapper or symlink
        # launch): the registry only resolves identities inside the fixtures
        # tree, so an argv[0]-based lookup must not pass for this entry.
        wrapper = base / 'wrapper-launch.py'
        environment = {**os.environ,
                       'CARGO_TARGET_DIR': str(base / 'cargo-target'),
                       'PDRS_BASELINE': str(base / 'pinned-ts-checkout')}
        try:
            with patch.dict(os.environ, environment), \
                 patch.object(module, 'mutating_sources', stubbed_window), \
                 patch.object(module, 'write_source_atomic', fake_write_atomic), \
                 patch.object(module, 'wait_for_slot', fake_slot), \
                 patch.object(module, 'run_owned', fake_run_owned), \
                 patch.object(module, 'passed', lambda result, test: True), \
                 patch.object(module, 'classify',
                              lambda result, case: ('KILLED',
                                                    'bound test failed at its assertion')), \
                 patch.object(module, 'select', dropping_select), \
                 patch.object(module, 'EXPECTED_NAMES',
                              module.EXPECTED_NAMES[:-1] if shrink_roster
                              else module.EXPECTED_NAMES), \
                 patch('sys.argv', [str(wrapper), str(out), *argv]), \
                 contextlib.redirect_stdout(stdout), \
                 contextlib.redirect_stderr(stderr):
                try:
                    # fix-r3 (DS concurrency P2): hold the same tree lock a
                    # real run would. The stubbed window still reads live
                    # source bytes, so a concurrently mutating batch on this
                    # checkout would hand the real ``mutate`` a mutant
                    # instead of the anchor and turn this pin red for no
                    # defect of the entry under test.
                    if registry is not None:
                        with patch.object(module, 'pending', registry), \
                                held_tree_lock(module.root):
                            code = module.main()
                    else:
                        with held_tree_lock(module.root):
                            code = module.main()
                except ValueError as error:
                    # A registry problem must be a refused start (rc 2), never
                    # an exception raised through main(); surface it as a code
                    # the assertions below can reject by name.
                    code = f'uncaught ValueError: {error}'
            # Sampled before the restore below puts the interpreter's own
            # dispositions back.
            installed = signal.getsignal(signal.SIGTERM) is module.terminate_cleanly
        finally:
            for name, handler in saved_handlers.items():
                # getsignal returns None when the disposition is held by
                # non-Python code, and signal.signal(..., None) would raise
                # TypeError (same guard as test_safety.py's restore site);
                # restore SIG_DFL there so the cleanup cannot mask the real
                # result with an unrelated exception (DS/anthropic 同撞).
                signal.signal(getattr(signal, name),
                              handler if handler is not None else signal.SIG_DFL)
        return module, code, out, stdout.getvalue(), stderr.getvalue(), writes, installed

    def test_full_run_publishes_only_after_passing_the_roster_recheck(self):
        module, code, out, stdout, stderr, writes, installed = self.drive([])
        self.assertTrue(code == 0,
                        f'#2309 the clean full run must exit 0, got {code}; '
                        f'stderr={stderr!r}')
        summary = json.loads((out / 'summary.json').read_text())
        rows = summary['controls']
        self.assertTrue(len(rows) == FULL_ROSTER,
                        f'#2309 the full run must record {FULL_ROSTER} rows, got '
                        f'{len(rows)}')
        self.assertTrue(all(row['status'] == 'KILLED' for row in rows),
                        '#2309 every stubbed-kill row must be recorded KILLED')
        selected = summary.get('selected')
        self.assertTrue(selected == list(module.EXPECTED_NAMES),
                        '#2309 a full run must record the whole roster as selected')
        self.assertTrue(not (out / 'roster-mismatch.json').exists(),
                        '#2309 a matching roster must not leave roster-mismatch.json')
        self.assertTrue('summary: ' in stdout,
                        '#2309 the green count line must follow the re-check')
        # Every write to a shared product source must go through the atomic
        # writer -- a truncating write_text/write_bytes mutant publish or
        # restore is the review's SIGKILL half-file hazard. The stub records
        # every call: a clean full run makes exactly one mutant write and one
        # restore per case, so a reintroduced in-place write drops the count.
        self.assertTrue(len(writes) == 2 * FULL_ROSTER,
                        f'#2309 each case must publish its mutant and restore '
                        f'through the atomic writer, got {len(writes)} calls')
        self.assertTrue(all(path.is_relative_to(module.root)
                            for path, _data in writes),
                        '#2309 the atomic writes must target the guarded tree only')
        # The self-held window must hold terminate handlers for the whole
        # run, not only inside the shared guard's own restore window
        # (#2309 fix-r3, the mode-routing precedent).
        self.assertTrue(installed,
                        '#2309 main() must install its terminate handler for '
                        'SIGTERM while the window is self-held')

    def test_a_shrunken_run_publishes_no_green_summary(self):
        module, code, out, stdout, stderr, _writes, _installed = self.drive([], drop_last=True)
        self.assertTrue(code == 1,
                        f'#2309 a run that lost a row must exit 1, got {code}')
        self.assertTrue(not (out / 'summary.json').exists(),
                        '#2309 a shrunken run must not publish summary.json')
        self.assertTrue('summary: ' not in stdout,
                        '#2309 a shrunken run must not print a green count line')
        self.assertTrue('verdict roster mismatch' in stderr,
                        f'#2309 the refusal must name the mismatch: {stderr!r}')
        mismatch = json.loads((out / 'roster-mismatch.json').read_text())
        dropped = module.EXPECTED_NAMES[-1]
        self.assertTrue(dropped in mismatch['expected'],
                        f'#2309 the dropped name must appear in expected: {dropped}')
        self.assertTrue(dropped not in mismatch['recorded'],
                        f'#2309 the dropped name must be absent from recorded: {dropped}')

    def test_a_subset_run_records_its_selection(self):
        names = ['invalid-params-fallback', 'error-mapped-to-response']
        module, code, out, stdout, stderr, _writes, _installed = self.drive(
            ['--only', ','.join(names)])
        self.assertTrue(code == 0,
                        f'#2309 the named subset must run green, got {code}; '
                        f'stderr={stderr!r}')
        summary = json.loads((out / 'summary.json').read_text())
        self.assertTrue(len(summary['controls']) == 2,
                        f'#2309 the subset run must record 2 rows, got '
                        f'{len(summary["controls"])}')
        selected = summary.get('selected')
        self.assertTrue(selected == names,
                        f'#2309 the green summary must record the executed set in '
                        f'table order, got {selected!r}')
        self.assertTrue(not (out / 'roster-mismatch.json').exists(),
                        '#2309 a complete subset must not leave roster-mismatch.json')

    def test_a_drifted_roster_never_starts(self):
        module, code, out, stdout, stderr, _writes, _installed = self.drive([], shrink_roster=True)
        self.assertTrue(code == 2,
                        f'#2309 a drifted roster must refuse to start (2), got {code}')
        self.assertTrue(not out.exists(),
                        '#2309 a refusal must not claim the evidence directory')
        self.assertTrue('roster drifted' in stderr,
                        f'#2309 the refusal must name the drift: {stderr!r}')

    def test_the_registry_identity_is_the_entry_itself(self):
        registry = RecordingRegistry(pending)
        module, code, out, stdout, stderr, _writes, _installed = self.drive([], registry=registry)
        self.assertTrue(code == 0,
                        f'#2309 the clean run must exit 0, got {code}; '
                        f'stderr={stderr!r}')
        # The identity key must be the entry's own path, never sys.argv[0]:
        # this driver launches the entry through a synthetic argv, and the
        # registry only resolves identities inside the fixtures tree.
        self.assertTrue(registry.calls == [ENTRY.resolve()],
                        f'#2309 for_script must receive the entry path, got '
                        f'{registry.calls!r}')

    def test_an_unusable_registry_refuses_before_claiming_the_directory(self):
        module, code, out, stdout, stderr, _writes, _installed = self.drive(
            [], registry=UnusableRegistry())
        self.assertTrue(code == 2,
                        f'#2309 an unusable registry is a refusal to start (2), '
                        f'got {code}')
        self.assertTrue(not out.exists(),
                        '#2309 the refusal must not claim the evidence directory')
        self.assertTrue('registry is not usable' in stderr,
                        f'#2309 the refusal must name the registry on stderr: '
                        f'{stderr!r}')


class SiblingRosterDefenceAnchorPins(unittest.TestCase):
    """The same defence in the sibling entries, pinned by ordered anchors.

    The behavioural drive above covers manager-gateway only: memory-gateway
    and snapshot-projection write their mutants with bare ``Path.write_text``
    inside the snapshot window, so driving their ``main()`` on a stubbed
    window would still write the guarded tree. The anchors below pin the
    same defence shape in source, and each test re-applies two kill demos to
    a doctored copy of its own source to prove the pin bites.
    """

    def entry_source(self, name):
        # utf-8 explicitly: sdk-mcp carries non-ASCII prose and a bare
        # read_text() would decode with the locale default (round 3, MM
        # boundary#8).
        return (FIXTURES / name / 'negative_controls.py').read_text(
            encoding='utf-8')

    def check_anchors(self, name, source):
        # Line-anchored, order-checked, comment-rejecting (#2773 fix-r3,
        # review MM r3 boundary#6 / DS boundary#1): a bare ``source.find``
        # lets a commented-out copy of the defence satisfy the chain while
        # the real code is gone. Each anchor must appear, in order, on a
        # line that is not a comment -- a copy inside a docstring or string
        # literal still counts, the filter only rejects ``#``-prefixed
        # lines (#2773 fix-r6, review DS r6 sibling#3 / boundary#3 /
        # simplify#3); the full AST relocation of the scan is the #2804
        # extension.
        lines = source.splitlines()
        position = 0
        for anchor in SIBLING_DEFENCE_ANCHORS[name]:
            found = None
            while position < len(lines):
                candidate = lines[position]
                position += 1
                if anchor in candidate and not candidate.lstrip().startswith('#'):
                    found = position - 1
                    break
            self.assertIsNotNone(
                found,
                f'#2773 fix-r3 {name}: the verify-before-publish roster '
                f'defence is broken at or moved before {anchor!r} -- the '
                f'refusal must run before anything that reads as success '
                f'(commented-out or out-of-order copies do not count)')

    def test_the_anchor_table_matches_the_declared_sibling_set(self):
        # The static denominator (#2773 fix-r3, review MM r3 boundary#1):
        # the table's keys are the loop domain, so they must equal the
        # declared literal -- deleting a key silently shortens every other
        # test in this class while the table stays green.
        self.assertEqual(
            tuple(SIBLING_DEFENCE_ANCHORS), EXPECTED_SIBLINGS,
            '#2773 fix-r3 the pinned sibling set must equal the declared '
            'denominator EXPECTED_SIBLINGS')

    def test_every_sibling_entry_keeps_the_defence(self):
        for name in EXPECTED_SIBLINGS:
            with self.subTest(entry=name):
                self.check_anchors(name, self.entry_source(name))

    def test_the_pin_kills_a_disabled_defence(self):
        # Two kill demos on doctored copies of each entry's own source
        # (#2773 fix-r3, review round 2 + round 3 MM boundary#6 / DS
        # boundary#1): the predicate flipped to a constant and every
        # anchor-bearing line commented out must both break the chain, so
        # the pin cannot rot into a no-op grep -- nor into one satisfied by
        # comments while the real defence is gone.
        for name in EXPECTED_SIBLINGS:
            with self.subTest(entry=name, doctor='predicate-const'):
                source = self.entry_source(name)
                self.assertEqual(
                    source.count('if recorded != expected:'), 1,
                    f'#2773 fix-r3 {name}: the kill demo needs exactly one '
                    f'predicate occurrence to doctor, found '
                    f'{source.count("if recorded != expected:")}')
                with self.assertRaises(AssertionError):
                    self.check_anchors(name, source.replace(
                        'if recorded != expected:', 'if False:', 1))
            with self.subTest(entry=name, doctor='defence-commented-out'):
                anchors = SIBLING_DEFENCE_ANCHORS[name]
                commented = '\n'.join(
                    f'# {line}' if any(anchor in line for anchor in anchors)
                    else line
                    for line in self.entry_source(name).splitlines())
                with self.assertRaises(AssertionError):
                    self.check_anchors(name, commented)


if __name__ == '__main__':
    unittest.main()
