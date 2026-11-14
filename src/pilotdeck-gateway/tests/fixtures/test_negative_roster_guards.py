#!/usr/bin/env python3
# anchor-selfcheck: unsupported meta-test of the roster verify-before-publish guard and the #2425 mutation-window mirror pins, drives every registered entry's main() with a stubbed cargo
"""#2804: the verify-before-publish roster check needs a denominator per entry. #2743 adds the mode-routing / session-domain-cleanup mutation-window mirror pins.

#2309 fix-r3 machine-checked one entry only: ``manager-gateway`` re-checks
the recorded verdicts against the static ``EXPECTED_NAMES`` roster *before*
publishing anything that reads as success, and ``test_negative_roster_guards``
drove that one ``main()`` with a stubbed executor layer. The same-shaped
defence existed in more entries still -- memory-gateway and
sdk-mcp-session-tools with no machine check at all, snapshot-projection
delivered through #1456 with one-shot sandbox demos only -- so constant-ifying
``recorded != expected``, dropping one ``EXPECTED_NAMES`` entry or deleting
the ``roster-mismatch.json`` branch kept every gate green there.

This file turns the hard-coded entry into a register (the
``BASELINE_GUARDED`` / ``TARGET_OPTIONAL`` shape of
``test_negative_optimize_guards.py``): ``ROSTER_GUARDED`` lists every entry
that carries the defence, a scan keeps the register honest in both
directions (an entry that publishes ``roster-mismatch.json`` must be listed,
a listed entry must really exist), and the same kill set runs per entry.
Each driver loads the real entry source (never a copy) and runs its real
``main()`` with only the executor layer stubbed. Two families exist:

* manager-gateway / sdk-mcp-session-tools / issue-2495 mutate through the
  shared ``mutating_sources`` window and ``write_source_atomic`` -- those, plus
  ``wait_for_slot`` / ``run_owned`` and the pass/kill judges, are stubbed;
  ``registered`` / selection, the real anchors, the roster re-check and
  every publish go through the entry's real code.
* memory-gateway / snapshot-projection publish their mutants with plain
  ``Path.write_text`` / ``Path.write_bytes`` and call module-level
  ``run_bound_test`` / ``run_test`` / ``compile_check`` -- those are stubbed,
  and guarded-path writes are recorded instead of touching the tree (the
  evidence logs, the summary and the mismatch file land for real in the
  temporary evidence directory). A guarded ``write_text`` is a mutant
  publish (the next bound run is the negative), a guarded ``write_bytes``
  its restore (the next bound run is the re-verify), so baseline and
  restored runs report green and mutant runs report the kill.

No cargo, no slot, no tree mutation: the stubbed window hands out the real
bytes it would snapshot. A stale anchor in an entry's table turns the
full-run case red here (the real anchor count refuses), which is the
desired fail-closed direction.

The injected mismatch differs per family because the entries' own seams
differ: manager-gateway loses the last selected row through a patched
``select`` (the #1666 hazard), while every other registered entry cannot lose a row
without failing their startup roster first, so the first stubbed cargo call
drops the last ``EXPECTED_NAMES`` name mid-run -- the run then records a
verdict the static roster no longer knows, the same ``recorded != expected``
predicate the guard exists to enforce. Every family also carries the
same-length form (``renamed=True``: the last roster name is changed in
place, so the recorded and expected lists disagree without any length
difference) and the same-multiset form (``reordered=True``: the roster
keeps every name and only their order rotates), because the entries'
predicate compares the ordered list of names, not counts and not sets.

The kill variants this register is designed to catch, each demonstrated
red/green in the delivery evidence: replacing ``if recorded != expected:``
with ``if False:`` (a mismatched run publishes a green summary and exits 0),
dropping one ``EXPECTED_NAMES`` entry (the startup roster refuses, rc 2,
no evidence directory), deleting the whole mismatch branch (a mismatched
run publishes summary.json and exits 0), deleting one register row (the
scan below finds the unregistered defence and fails), and weakening the
predicate to a length comparison (the rename case above survives it, so
the rename case must exit 1), and weakening it to a sorted or set
comparison (the reorder case above survives it, so the reorder case must
exit 1).

#2743: the same two #2425 mutation-window pins (every source byte goes out
through the atomic writer; the self-held window runs under terminate
handlers) had landed for same-json-surfaces in the frozen
``scripts/gateway-parity/test_safety.py`` and for manager-gateway here, but
``mode-routing`` and ``session-domain-cleanup`` -- the two same-shape
guarded runners the #1355 r20 anthropic sibling#1 review named -- had no
mirror. ``ModeRoutingGuard`` and ``SessionDomainCleanupGuard`` below close
that gap with the same stubbed-executor drive; the session-domain-cleanup
fixture itself also had to *adopt* ``write_source_atomic`` first (its
publish/restore were still truncating ``write_text``/``write_bytes``, the
exact #1903 SIGKILL half-file hazard #2425 pinned, in the one guarded
runner that mutates frozen ``scripts/gateway-parity`` bytes under the
parity lease). The session-domain-cleanup count has one extra term: its
window-exit backstop rewrites every guarded source once more after the
loop, so a clean full run records two atomic writes per mutation plus one
per guarded source.

Run from this directory: ``python3 -m unittest test_negative_roster_guards``.
"""
import contextlib
import importlib.util
import io
import json
import os
from pathlib import Path
import pathlib
import re
import shutil
import signal
import subprocess
import sys
import tempfile
import time
import unittest
from unittest.mock import patch

import pending_reanchor as pending
from negative_process import tree_lock

FIXTURES = Path(__file__).resolve().parent
# The tree the guarded drivers lock: the same ``<root>/.negative-source.lock``
# their own ``mutating_sources`` flocks (a driver's ROOT is parents[5] from
# its deeper fixture nesting; from this file it is parents[4]).
REPO_ROOT = Path(__file__).resolve().parents[4]
# fix-r11 (P2, DS sibling#1 ≡ MM sibling#1, the pin fixture's twin): the
# depth constant must be self-proved -- the pin fixture got its proof in
# fix-r10 but this twin (one level shallower, ``parents[4]``) stayed
# bare. Same external anchor, same explicit raise (not an assert --
# ``python -O`` strips asserts), not a second hand-rolled scheme: without
# it a relocated or flattened fixture tree would derive some other
# *existing* directory and ``tree_lock`` (``os.open(..., O_CREAT)``)
# would silently create a stranger's lock file -- run_copy's and
# _drive_2495's windows would stay green while locking nothing at all.
if not (REPO_ROOT / 'src/Cargo.toml').is_file():
    raise RuntimeError(
        '#3043 REPO_ROOT must be the repository root (the directory '
        f'holding src/Cargo.toml), got {REPO_ROOT}: a relocated or '
        'flattened fixture tree would silently lock a stranger\'s tree')
FULL_ROSTER = 36
# mode-routing's EXPECTED_NAMES literal (that fixture's negative_controls
# :164-175): the roster count pinned independently of module.EXPECTED_NAMES,
# the same shape as FULL_ROSTER for manager-gateway (#1666 family).
MODE_ROSTER = 10
# The close-order-reader table's EXPECTED_NAMES length (#3043 fix-r4 P1,
# the same #1666-family second denominator MODE_ROSTER is): the #3043
# fourth-cell fixture registered its roster with the shared
# ``negative_selection.registered()`` startup check (fix-r2), but that
# compares the runtime ROWS against the module's *own* static tuple --
# deleting a row together with its EXPECTED_NAMES entry shrinks both sides
# and the run still publishes a (N-1)/(N-1) all-KILLED exit 0. Only this
# literal, which the table under test cannot influence, refuses it.
CLOSE_ORDER_READER_ROSTER = 6
# The shared-runner table's EXPECTED_NAMES length (#3043 fix-r6, DS r5
# sibling P1 #3134 -- the same #1666-family second denominator the sdk
# -signals StaticRosterTests got in fix-r5): the shared-runner fixture
# registered its MUTANT_ROWS with ``negative_selection.registered()`` but
# had no second denominator and no test case for the refusal, so a rename,
# a reorder, or a row deleted together with its EXPECTED_NAMES entry kept
# every check green (measured pre-fix: a single-deleted row still printed
# ``38 anchors: ok``). Only this literal, which the table under test
# cannot influence, refuses the double-delete direction.
SHARED_RUNNER_ROSTER = 39
TERMINATE_SIGNALS = ('SIGTERM', 'SIGHUP', 'SIGQUIT')
MANAGER_SCRIPT = 'manager-gateway/negative_controls.py'
ENTRY = FIXTURES / MANAGER_SCRIPT

# Every register row's full ``EXPECTED_NAMES`` length, pinned to a literal
# the module under test cannot influence (fix-r10, DS r9 + MM r9 sibling P1
# -- the same #1666 family shape MODE_ROSTER and the session-domain 331 pin
# closed in r9). The full-run mirror used to compare ``run.module.
# EXPECTED_NAMES`` to itself: deleting a mutations row together with its
# EXPECTED_NAMES entry shrinks both sides of the mirror and keeps it green.
# These are the full roster lengths -- ``registered`` compares the whole
# list, it never tail-cuts sentinel names -- measured against the source
# literals, so a register row must not drift its roster without failing
# here. A new register row joins this table in the same change (the
# existence test below enforces the coverage in both directions).
ROSTER_SIZE = {
    MANAGER_SCRIPT: FULL_ROSTER,
    'memory-gateway/negative_controls.py': 18,
    'snapshot-projection/negative_controls.py': 20,
    'sdk-mcp-session-tools/negative_controls.py': 9,
    'issue-2495/negative_controls.py': 7,
}

# The defence's own signature in an entry's source: a mismatched run
# publishes a file named roster-mismatch.json and nothing else. Matching
# both quote styles keeps the register honest the way BASELINE_READ does
# for the baseline guard (#2309 fix-r3 precedent). The scan covers every
# ``negative_controls.py`` at any depth; same-shaped entries with other
# file names follow the optimize-guards register's scoping precedent.
ROSTER_MISMATCH_WRITE = re.compile(r'''['"]roster-mismatch\.json['"]''')


def load_entry(script):
    """Import the real entry under test (the source file is the truth, never a copy)."""
    entry = FIXTURES / script
    spec = importlib.util.spec_from_file_location(
        'roster_guard_' + script[:-3].replace('/', '_').replace('-', '_'), entry)
    module = importlib.util.module_from_spec(spec)
    saved = sys.path[:]
    try:
        spec.loader.exec_module(module)
    finally:
        # The entries prepend their import roots at module level; keep the
        # test interpreter's path as it was.
        sys.path[:] = saved
    return module


def _shrunk(names):
    """The roster with one name deleted, the way a dropped row drifts it."""
    return names[:1] + names[2:]


def _environment(base):
    return {**os.environ,
            'CARGO_TARGET_DIR': str(base / 'cargo-target'),
            'PDRS_BASELINE': str(base / 'pinned-ts-checkout')}


def _ok_log(test):
    """A libtest transcript the real ``baseline_passed`` accepts."""
    return f'test {test} ... ok\ntest result: ok. 1 passed; 0 failed;\n'


def _call_main(module):
    """Run the entry's real ``main()``; a refusal may exit instead of returning."""
    try:
        return module.main()
    except SystemExit as error:
        # sdk-mcp-session-tools refuses through negative_selection.refuse
        # (SystemExit 2); surface the int code for the assertions to judge.
        if isinstance(error.code, int):
            return error.code
        raise
    except ValueError as error:
        # A registry problem must be a refused start (rc 2), never an
        # exception raised through main(); surface it as a value the
        # assertions below can reject by name.
        return f'uncaught ValueError: {error}'


class Drive:
    """One driven run of an entry's real ``main()``."""

    def __init__(self, module, code, out, stdout, stderr, writes, installed):
        self.module = module
        self.code = code
        self.out = out
        self.stdout = stdout
        self.stderr = stderr
        self.writes = writes
        # True when main() held its terminate handlers over the self-held
        # window (manager-gateway only; the other families install none).
        self.installed = installed


@contextlib.contextmanager
def stubbed_window(root, paths, *, baseline=None, lock_timeout=None):
    """The real snapshot window refuses a dirty tree by design; hand out bytes.

    The test tree carries the fix under review, so the pristine-vs-HEAD check
    would refuse every run. The bytes are the real current sources -- the
    real anchor checks below still walk every real anchor.
    """
    yield {path: path.read_bytes() for path in paths}


@contextlib.contextmanager
def _shadowed_source_writes(guarded, writes, phase):
    """Record the guarded-source writes of the write_text/write_bytes family.

    memory-gateway and snapshot-projection publish their mutants with plain
    ``Path.write_text`` and restore with ``Path.write_bytes`` on the guarded
    sources. A write to a guarded path is recorded and never touches the
    tree (the mutant and its restore cancel out, and every later consumer
    here is stubbed); every other write -- evidence logs, the summary, the
    mismatch file -- lands for real in the temporary evidence directory.
    A guarded ``write_text`` moves the phase to the mutant run, a guarded
    ``write_bytes`` back to the restored run, so the stubbed bound tests
    can tell the negative from the baseline and the re-verify.
    """
    real_write_text = pathlib.Path.write_text
    real_write_bytes = pathlib.Path.write_bytes

    def recording_write_text(self, data, *args, **kwargs):
        if self.resolve() in guarded:
            writes.append((self, data.encode(kwargs.get('encoding') or 'utf-8')
                           if isinstance(data, str) else bytes(data)))
            phase[0] = 'mutant'
            return len(data)
        return real_write_text(self, data, *args, **kwargs)

    def recording_write_bytes(self, data):
        if self.resolve() in guarded:
            writes.append((self, bytes(data)))
            phase[0] = 'restored'
            return len(data)
        return real_write_bytes(self, bytes(data))

    with patch.object(pathlib.Path, 'write_text', recording_write_text), \
         patch.object(pathlib.Path, 'write_bytes', recording_write_bytes):
        yield


def _drive_manager(testcase, argv, *, drop_last=False, renamed=False,
                   reordered=False, shrink_roster=False, registry=None):
    """Run manager-gateway's real ``main()``; return the Drive of that run."""
    module = load_entry(MANAGER_SCRIPT)
    base = Path(tempfile.mkdtemp(prefix='negctl-2804-'))
    testcase.addCleanup(shutil.rmtree, base, ignore_errors=True)
    out = base / 'out'
    writes = []
    real_select = module.select

    def dropping_select(controls, requested, **kwargs):
        rows = real_select(controls, requested, **kwargs)
        if drop_last and rows:
            # The injected shrink: a select() that silently loses the last
            # row is the #1666 hazard this guard exists to catch.
            return rows[:-1]
        if renamed and rows:
            # The injected rename: the row keeps its place, so the recorded
            # roster keeps its length, but its last name is no longer the
            # name the roster pins. A re-check that only compared lengths
            # would wave this through; the entries compare names.
            rows = list(rows)
            last = rows[-1]
            rows[-1] = (last[0] + '-renamed',) + last[1:]
        if reordered and rows:
            # The injected reorder: the rows keep their names and count, so
            # a re-check that compared sorted lists or sets would wave this
            # through; the entries' predicate compares the list in order.
            rows = list(rows)
            rows[0], rows[-1] = rows[-1], rows[0]
        return rows

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
    window_entry = {}
    window_exit = {}

    @contextlib.contextmanager
    def recording_window(root, paths, **kwargs):
        # The strong shape the two new classes in this change already pin
        # (#2425, frozen test_safety.py:2862-2900): record the live
        # dispositions when the window opens, and require every
        # TERMINATE_SIGNALS member to be bound. A late install -- after the
        # window entry -- leaves the batch unprotected while a post-return
        # sample would still see the handler; the SIGTERM-only shape was
        # judged P1 on the frozen side once already (DS rebase review of
        # this PR, sibling#1). Sample again when the window closes: a
        # mid-window restore to the default dispositions must stay red
        # even though the entry sample was clean (DS r5 sibling#1 /
        # boundary#1 -- two reviewers independently hit the entry-only
        # hole; entry+exit together catch both the late install and the
        # early restore).
        window_entry.update({name: signal.getsignal(getattr(signal, name))
                             for name in TERMINATE_SIGNALS
                             if hasattr(signal, name)})
        try:
            yield {path: path.read_bytes() for path in paths}
        finally:
            window_exit.update({name: signal.getsignal(getattr(signal, name))
                                for name in TERMINATE_SIGNALS
                                if hasattr(signal, name)})

    installed = False
    stdout, stderr = io.StringIO(), io.StringIO()
    # A launcher identity that is NOT the entry path (a wrapper or symlink
    # launch): the registry only resolves identities inside the fixtures
    # tree, so an argv[0]-based lookup must not pass for this entry.
    wrapper = base / 'wrapper-launch.py'
    try:
        with patch.dict(os.environ, _environment(base)), \
             patch.object(module, 'mutating_sources', recording_window), \
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
                if registry is not None:
                    with patch.object(module, 'pending', registry):
                        code = module.main()
                else:
                    code = module.main()
            except ValueError as error:
                code = f'uncaught ValueError: {error}'
        # Judged from the window-entry AND window-exit recordings above,
        # before the restore below puts the interpreter's own dispositions
        # back: the handlers must be in place when the batch starts and
        # still be in place when the window closes.
        installed = bool(window_entry) and bool(window_exit) and all(
            handler is module.terminate_cleanly
            for handler in window_entry.values()) and all(
            handler is module.terminate_cleanly
            for handler in window_exit.values())
    finally:
        for name, handler in saved_handlers.items():
            # getsignal returns None when a non-Python handler held the
            # signal; signal.signal refuses None (frozen side fixed this in
            # test_safety.py:2834-2840; DS concurrency #3).
            signal.signal(getattr(signal, name),
                          handler if handler is not None else signal.SIG_DFL)
    return Drive(module, code, out, stdout.getvalue(), stderr.getvalue(), writes, installed)


def _rename_roster(names):
    """The same-length drift: the roster keeps its size, not its last name."""
    renamed = list(names)
    renamed[-1] += '-renamed'
    return tuple(renamed)


def _reorder_roster(names):
    """The same-multiset drift: the roster keeps every name and its length,
    not their order (the last name rotates to the front)."""
    return tuple(names[-1:]) + tuple(names[:-1])


def _drive_memory(testcase, *, perturbed=False, renamed=False,
                  reordered=False, shrink_roster=False, argv=()):
    """Run memory-gateway's real ``main()``; return the Drive of that run."""
    module = load_entry('memory-gateway/negative_controls.py')
    base = Path(tempfile.mkdtemp(prefix='negctl-2804-'))
    testcase.addCleanup(shutil.rmtree, base, ignore_errors=True)
    out = base / 'out'
    writes = []
    guarded = set()
    phase = ['baseline']
    drifted = [False]

    @contextlib.contextmanager
    def window(root, paths, *, baseline=None, lock_timeout=None):
        # The same contract as the manager family's stubbed window; the
        # guarded set is what the write shadow below records instead of
        # touching the tree.
        for path in paths:
            guarded.add(path.resolve())
        yield {path: path.read_bytes() for path in paths}

    def fake_run_bound(test):
        if (perturbed or renamed or reordered) and not drifted[0]:
            # The injected drift: after the startup checks, the static
            # roster loses its last name, keeps its length but not that
            # name, or keeps every name and only their order -- so the run
            # records verdicts the roster no longer pins, the same
            # recorded != expected predicate, at the only seam a driver
            # can reach.
            drifted[0] = True
            module.EXPECTED_NAMES = (_rename_roster(module.EXPECTED_NAMES)
                                     if renamed
                                     else _reorder_roster(module.EXPECTED_NAMES)
                                     if reordered
                                     else module.EXPECTED_NAMES[:-1])
        if phase[0] == 'mutant':
            return subprocess.CompletedProcess(
                ['stub'], 1,
                f'test {test} ... FAILED\nfailures:\n    the bound assertion fired\n'
                f'test result: FAILED. 0 passed; 1 failed;\n', '')
        return subprocess.CompletedProcess(['stub'], 0, _ok_log(test), '')

    def fake_compile():
        return subprocess.CompletedProcess(['compile-check'], 0, '', '')

    stdout, stderr = io.StringIO(), io.StringIO()
    wrapper = base / 'wrapper-launch.py'
    with patch.dict(os.environ, _environment(base)), \
         patch.object(module, 'mutating_sources', window), \
         patch.object(module, 'run_bound_test', fake_run_bound), \
         patch.object(module, 'compile_check', fake_compile), \
         patch.object(module, 'named_test_killed', lambda log, test, marker: True), \
         patch.object(module, 'EXPECTED_NAMES',
                      _shrunk(module.EXPECTED_NAMES) if shrink_roster
                      else module.EXPECTED_NAMES), \
         patch('sys.argv', [str(wrapper), str(out), *argv]), \
         contextlib.redirect_stdout(stdout), \
         contextlib.redirect_stderr(stderr), \
         _shadowed_source_writes(guarded, writes, phase):
        code = _call_main(module)
    return Drive(module, code, out, stdout.getvalue(), stderr.getvalue(), writes, None)


def _drive_snapshot(testcase, *, perturbed=False, renamed=False,
                    reordered=False, shrink_roster=False, argv=()):
    """Run snapshot-projection's real ``main()``; return the Drive of that run."""
    module = load_entry('snapshot-projection/negative_controls.py')
    base = Path(tempfile.mkdtemp(prefix='negctl-2804-'))
    testcase.addCleanup(shutil.rmtree, base, ignore_errors=True)
    out = base / 'out'
    writes = []
    guarded = set()
    phase = ['baseline']
    drifted = [False]

    @contextlib.contextmanager
    def window(root, paths, *, baseline=None, lock_timeout=None):
        for path in paths:
            guarded.add(path.resolve())
        yield {path: path.read_bytes() for path in paths}

    def fake_run(target, test, env):
        if (perturbed or renamed or reordered) and not drifted[0]:
            drifted[0] = True
            module.EXPECTED_NAMES = (_rename_roster(module.EXPECTED_NAMES)
                                     if renamed
                                     else _reorder_roster(module.EXPECTED_NAMES)
                                     if reordered
                                     else module.EXPECTED_NAMES[:-1])
        if phase[0] == 'mutant':
            return subprocess.CompletedProcess(
                ['stub'], 1,
                f'test {test} ... FAILED\nfailures:\n    the bound assertion fired\n'
                f'test result: FAILED. 0 passed; 1 failed;\n', '')
        return subprocess.CompletedProcess(['stub'], 0, _ok_log(test), '')

    def fake_compile(target, env):
        return subprocess.CompletedProcess(['compile-check'], 0, '', '')

    def fake_node_version(env=None, **kwargs):
        # The generator guard's Node-22 prerequisite, stubbed so the meta
        # test does not depend on a host node: the judged run below is the
        # stubbed run_owned, not a real generator execution.
        return 'v22.22.0', None

    def fake_run_owned(command, **kwargs):
        # generator-guard-bypassed: the generator's ownership guard must
        # reject the bypass (the entry judges this line itself).
        return subprocess.CompletedProcess(
            command, 1,
            'Missing expected rejection: removeOwned refused the path\n', '')

    stdout, stderr = io.StringIO(), io.StringIO()
    wrapper = base / 'wrapper-launch.py'
    with patch.dict(os.environ, _environment(base)), \
         patch.object(module, 'mutating_sources', window), \
         patch.object(module, 'run_test', fake_run), \
         patch.object(module, 'compile_check', fake_compile), \
         patch.object(module, 'node_version', fake_node_version), \
         patch.object(module, 'run_owned', fake_run_owned), \
         patch.object(module, 'named_test_killed', lambda log, test, marker: True), \
         patch.object(module, 'EXPECTED_NAMES',
                      _shrunk(module.EXPECTED_NAMES) if shrink_roster
                      else module.EXPECTED_NAMES), \
         patch('sys.argv', [str(wrapper), str(out), *argv]), \
         contextlib.redirect_stdout(stdout), \
         contextlib.redirect_stderr(stderr), \
         _shadowed_source_writes(guarded, writes, phase):
        code = _call_main(module)
    return Drive(module, code, out, stdout.getvalue(), stderr.getvalue(), writes, None)


def _drive_sdk(testcase, *, perturbed=False, renamed=False,
              reordered=False, shrink_roster=False, argv=()):
    """Run sdk-mcp-session-tools' real ``main()``; return the Drive of that run."""
    module = load_entry('sdk-mcp-session-tools/negative_controls.py')
    base = Path(tempfile.mkdtemp(prefix='negctl-2804-'))
    testcase.addCleanup(shutil.rmtree, base, ignore_errors=True)
    out = base / 'out'
    writes = []
    drifted = [False]

    def fake_write_atomic(root, path, data, *, mode=None):
        writes.append((Path(path), bytes(data)))

    def fake_slot(script, **kwargs):
        return subprocess.CompletedProcess([], 0, '', '')

    def fake_run_owned(command, **kwargs):
        if (perturbed or renamed or reordered) and not drifted[0]:
            drifted[0] = True
            module.EXPECTED_NAMES = (_rename_roster(module.EXPECTED_NAMES)
                                     if renamed
                                     else _reorder_roster(module.EXPECTED_NAMES)
                                     if reordered
                                     else module.EXPECTED_NAMES[:-1])
        test = command[-4]
        return subprocess.CompletedProcess(
            command, 0,
            f'test {test} ... ok\ntest result: ok. 1 passed; 0 failed;\n', '')

    def fake_judge(result, raw, targets, receipt):
        return 'KILLED', 'bound test failed at its assertion', []

    stdout, stderr = io.StringIO(), io.StringIO()
    wrapper = base / 'wrapper-launch.py'
    with patch.dict(os.environ, _environment(base)), \
         patch.object(module, 'mutating_sources', stubbed_window), \
         patch.object(module, 'write_source_atomic', fake_write_atomic), \
         patch.object(module, 'wait_for_slot', fake_slot), \
         patch.object(module, 'run_owned', fake_run_owned), \
         patch.object(module, 'passed', lambda result, test: True), \
         patch.object(module, 'judge', fake_judge), \
         patch.object(module, 'EXPECTED_NAMES',
                      _shrunk(module.EXPECTED_NAMES) if shrink_roster
                      else module.EXPECTED_NAMES), \
         patch('sys.argv', [str(wrapper), str(out), *argv]), \
         contextlib.redirect_stdout(stdout), \
         contextlib.redirect_stderr(stderr):
        code = _call_main(module)
    return Drive(module, code, out, stdout.getvalue(), stderr.getvalue(), writes, None)


def _drive_2495(testcase, *, perturbed=False, renamed=False,
               reordered=False, shrink_roster=False, argv=()):
    """Run issue-2495's real ``main()``; return the Drive of that run.

    The #2590 migrated-family row (#1433 sdk shape): same executor seams as
    the sdk driver, and like the sdk row it leaves the startup target guards
    (``missing_target`` / ``refuse_unusable_target`` over ``_environment``'s
    temp ``CARGO_TARGET_DIR``) and the ``stale_names()`` anchor scan over
    the tree's real bytes unstubbed. The one thing this row adds over sdk is
    a real ``tree_lock`` around that scan: it stays the entry's own real
    lock, only with the wait bounded to 2 s (``GUARD_LOCK_FAST``, the
    entry's own fixture precedent) so a sibling round holding
    ``ROOT/.negative-source.lock`` makes this row fail closed in seconds
    instead of 60 s of silent wait per case. The ``--check`` /
    argv-contract refusals are also unstubbed, but this register never
    drives them — their coverage lives in the entry's own fixture
    (issue-2495/test_negative_controls.py).
    """
    module = load_entry('issue-2495/negative_controls.py')
    base = Path(tempfile.mkdtemp(prefix='negctl-2804-'))
    testcase.addCleanup(shutil.rmtree, base, ignore_errors=True)
    out = base / 'out'
    writes = []
    drifted = [False]
    real_tree_lock = module.tree_lock

    def fast_tree_lock(root, **kwargs):
        kwargs['timeout'] = 2
        return real_tree_lock(root, **kwargs)

    def fake_write_atomic(root, path, data, *, mode=None):
        writes.append((Path(path), bytes(data)))

    def fake_slot(script, **kwargs):
        return subprocess.CompletedProcess([], 0, '', '')

    def fake_run_owned(command, **kwargs):
        if (perturbed or renamed or reordered) and not drifted[0]:
            drifted[0] = True
            module.EXPECTED_NAMES = (_rename_roster(module.EXPECTED_NAMES)
                                     if renamed
                                     else _reorder_roster(module.EXPECTED_NAMES)
                                     if reordered
                                     else module.EXPECTED_NAMES[:-1])
        # The non-compile command carries the bound test name at -4; the
        # compile-only leg's log has no consumer here (passed/judge stubbed).
        return subprocess.CompletedProcess(command, 0, _ok_log(command[-4]), '')

    def fake_judge(result, raw, targets, receipt):
        return 'KILLED', 'bound test failed at its assertion', []

    stdout, stderr = io.StringIO(), io.StringIO()
    wrapper = base / 'wrapper-launch.py'
    with patch.dict(os.environ, _environment(base)), \
         patch.object(module, 'mutating_sources', stubbed_window), \
         patch.object(module, 'write_source_atomic', fake_write_atomic), \
         patch.object(module, 'wait_for_slot', fake_slot), \
         patch.object(module, 'run_owned', fake_run_owned), \
         patch.object(module, 'tree_lock', fast_tree_lock), \
         patch.object(module, 'passed', lambda result, test: True), \
         patch.object(module, 'judge', fake_judge), \
         patch.object(module, 'EXPECTED_NAMES',
                      _shrunk(module.EXPECTED_NAMES) if shrink_roster
                      else module.EXPECTED_NAMES), \
         patch('sys.argv', [str(wrapper), str(out), *argv]), \
         contextlib.redirect_stdout(stdout), \
         contextlib.redirect_stderr(stderr):
        code = _call_main(module)
    return Drive(module, code, out, stdout.getvalue(), stderr.getvalue(), writes, None)


def _drive_manager_entry(testcase, *, perturbed=False, renamed=False,
                         reordered=False, shrink_roster=False, argv=()):
    """The register's manager-gateway row: the perturbation is the lost,
    renamed or reordered row (``perturbed`` drops the last selected row
    through ``select``; ``renamed`` keeps the row but changes its name;
    ``reordered`` swaps the first and last selected rows)."""
    return _drive_manager(testcase, argv, drop_last=perturbed, renamed=renamed,
                          reordered=reordered, shrink_roster=shrink_roster)


# #2804: the entries whose main() re-checks the recorded verdict roster
# before publishing anything that reads as success. The register is the
# denominator of this file (the BASELINE_GUARDED / TARGET_OPTIONAL shape):
# a family entry that grows the same-shaped defence must join here in the
# same change, and a row may only be removed together with the defence.
# Pending absorption, same register shape, deliberately not implemented in
# this change: the terminate-handler "installed" static pin and the
# atomic-write "went through" static pin for mode-routing / same-json-
# surfaces (only scripts/gateway-parity/test_safety.py's single-fixture
# class pins those today, #2605 item 1 / #2577).
ROSTER_GUARDED = {
    MANAGER_SCRIPT: _drive_manager_entry,
    'memory-gateway/negative_controls.py': _drive_memory,
    'snapshot-projection/negative_controls.py': _drive_snapshot,
    'sdk-mcp-session-tools/negative_controls.py': _drive_sdk,
    # #2804: issue-2495 carries the same roster re-check (the mismatch
    # publish itself landed with #2590 fix-r3b4), so the scan requires it
    # registered the same as every other row.
    'issue-2495/negative_controls.py': _drive_2495,
}

# The lines each entry prints on its way out of a *clean* run. A mismatched
# run must leave none of them behind (r5: the shrunken run used to print
# the green count line after the summary was unlinked). The full-run case
# asserts each marker IS present on the clean run, so a wording change in
# an entry turns the register red instead of silently disarming the
# mismatch assertions.
SUCCESS_LINE = {
    MANAGER_SCRIPT: ('summary:',),
    'memory-gateway/negative_controls.py': ('summary:', ' killed; '),
    'snapshot-projection/negative_controls.py': (' KILLED; see ',),
    'sdk-mcp-session-tools/negative_controls.py': ('"controls"',),
    'issue-2495/negative_controls.py': ('summary:', '"controls"'),
}


class RegisteredEntryRosterGuard(unittest.TestCase):
    """The register is the denominator; every row carries the same kill set.

    Each test loops the register so a row cannot be added without inheriting
    the whole set, and the scan test keeps the register honest in both
    directions: an entry that grows the defence without registering fails
    the scan, and deleting a register row fails it the same way (the
    defence is still in the source but no longer machine-checked).
    """

    def test_registered_entries_exist(self):
        for script in ROSTER_GUARDED:
            with self.subTest(script=script):
                self.assertTrue((FIXTURES / script).is_file(),
                                f'#2804 {script} is registered but does not exist')
        # r3: the green-line register must not silently disarm itself. A
        # row with an empty tuple (or a missing row, or an empty-string
        # marker) would pass every positive and negative marker assertion
        # vacuously -- exactly the shape this file exists to refuse.
        self.assertTrue(set(SUCCESS_LINE) == set(ROSTER_GUARDED),
                        '#2804 SUCCESS_LINE must cover exactly the register '
                        f'(extra={sorted(set(SUCCESS_LINE) - set(ROSTER_GUARDED))} '
                        f'missing={sorted(set(ROSTER_GUARDED) - set(SUCCESS_LINE))})')
        # fix-r10: the roster-size literals must cover the register the same
        # way, so a row cannot join without its denominator pin and a stale
        # pin cannot outlive its row.
        self.assertTrue(set(ROSTER_SIZE) == set(ROSTER_GUARDED),
                        '#2804 ROSTER_SIZE must cover exactly the register '
                        f'(extra={sorted(set(ROSTER_SIZE) - set(ROSTER_GUARDED))} '
                        f'missing={sorted(set(ROSTER_GUARDED) - set(ROSTER_SIZE))})')
        for script, markers in SUCCESS_LINE.items():
            with self.subTest(script=script):
                self.assertTrue(len(markers) > 0,
                                f'#2804 {script}: SUCCESS_LINE must not be empty')
                for marker in markers:
                    self.assertTrue(marker,
                                    f'#2804 {script}: an empty-string marker '
                                    'asserts vacuously')

    def test_every_roster_guard_entry_is_registered(self):
        found = {str(path.relative_to(FIXTURES))
                 for path in FIXTURES.rglob('negative_controls.py')
                 if ROSTER_MISMATCH_WRITE.search(path.read_text(encoding='utf-8'))}
        self.assertTrue(found == set(ROSTER_GUARDED),
                        f'#2804 every entry that publishes roster-mismatch.json '
                        f'must be machine-checked through the register '
                        f'(unregistered={sorted(found - set(ROSTER_GUARDED))} '
                        f'stale={sorted(set(ROSTER_GUARDED) - found)})')

    def test_full_run_publishes_only_after_the_roster_recheck(self):
        for script, driver in ROSTER_GUARDED.items():
            with self.subTest(script=script):
                run = driver(self)
                self.assertTrue(run.code == 0,
                                f'#2804 {script}: the clean full run must exit 0, '
                                f'got {run.code}; stderr={run.stderr!r}')
                summary = json.loads((run.out / 'summary.json').read_text())
                rows = summary['controls'] if isinstance(summary, dict) else summary
                key = 'mutation' if script == MANAGER_SCRIPT else 'name'
                # Denominator pinned to the source literal first (fix-r10,
                # DS r9 + MM r9 sibling P1, #1666 family): the mirror below
                # compares run.module.EXPECTED_NAMES to itself, so a row
                # deleted from the table and the roster together shrinks
                # both sides and stays green. The literal must fail first.
                self.assertTrue(len(run.module.EXPECTED_NAMES)
                                == ROSTER_SIZE[script],
                                f'#2804 {script}: the entry must carry its '
                                f'literal {ROSTER_SIZE[script]}-name roster; '
                                'truncating or emptying EXPECTED_NAMES must '
                                'fail here instead of shrinking the mirror')
                # Order-sensitive, like the production predicate: a run that
                # records the right names in the wrong order is a mismatch
                # the entries' ``recorded != expected`` would refuse.
                names = [row[key] for row in rows]
                self.assertTrue(names == list(run.module.EXPECTED_NAMES),
                                f'#2804 {script}: the full run must record exactly '
                                f'the roster names in roster order, got {names}')
                for marker in SUCCESS_LINE[script]:
                    self.assertIn(marker, run.stdout,
                                  f'#2804 {script}: the clean run must print its '
                                  f'green line ({marker!r})')
                self.assertTrue(all(row['status'] == 'KILLED' for row in rows),
                                f'#2804 {script}: every stubbed-kill row must be '
                                f'recorded KILLED')
                self.assertTrue(not (run.out / 'roster-mismatch.json').exists(),
                                f'#2804 {script}: a matching roster must not leave '
                                f'roster-mismatch.json')

    def test_a_mismatched_run_publishes_no_green_summary(self):
        for script, driver in ROSTER_GUARDED.items():
            with self.subTest(script=script):
                run = driver(self, perturbed=True)
                self.assertTrue(run.code == 1,
                                f'#2804 {script}: a run whose verdicts disagree '
                                f'with the roster must exit 1, got {run.code}')
                self.assertTrue(not (run.out / 'summary.json').exists(),
                                f'#2804 {script}: a mismatched run must not publish '
                                f'summary.json')
                mismatch = json.loads((run.out / 'roster-mismatch.json').read_text())
                self.assertTrue(mismatch.get('verdict') == 'ROSTER_MISMATCH',
                                f'#2804 {script}: the mismatch artifact must carry '
                                f'the verdict: {mismatch!r}')
                self.assertTrue(set(mismatch['expected']) != set(mismatch['recorded']),
                                f'#2804 {script}: the mismatch artifact must name '
                                f'the disagreement: {mismatch!r}')
                self.assertTrue('verdict roster mismatch' in run.stderr,
                                f'#2804 {script}: the refusal must name the '
                                f'mismatch: {run.stderr!r}')
                # A mismatch publishes nothing that reads as success, and
                # that includes the green count line each entry prints on
                # the way out of a clean run (r5: the shrunken run used to
                # leave one behind after the summary was unlinked).
                for marker in SUCCESS_LINE[script]:
                    self.assertTrue(marker not in run.stdout,
                                    f'#2804 {script}: a mismatched run must '
                                    f'not print its green line ({marker!r}): '
                                    f'{run.stdout!r}')

    def test_a_same_length_rename_is_still_a_mismatch(self):
        # The recorded roster keeps its length and only a name changes, so
        # a re-check that only compared lengths would wave it through; the
        # entries' recorded != expected compares names (boundary P1,
        # fix-r1).
        for script, driver in ROSTER_GUARDED.items():
            with self.subTest(script=script):
                run = driver(self, renamed=True)
                self.assertTrue(run.code == 1,
                                f'#2804 {script}: a run whose recorded names '
                                f'disagree with the roster must exit 1, got '
                                f'{run.code}')
                self.assertTrue(not (run.out / 'summary.json').exists(),
                                f'#2804 {script}: a mismatched run must not publish '
                                f'summary.json')
                mismatch = json.loads((run.out / 'roster-mismatch.json').read_text())
                self.assertTrue(mismatch.get('verdict') == 'ROSTER_MISMATCH',
                                f'#2804 {script}: the mismatch artifact must carry '
                                f'the verdict: {mismatch!r}')
                self.assertTrue(len(mismatch['expected']) == len(mismatch['recorded']),
                                f'#2804 {script}: the rename keeps the roster '
                                f'length, so the artifact must show two '
                                f'same-length lists: {mismatch!r}')
                self.assertTrue(set(mismatch['expected']) != set(mismatch['recorded']),
                                f'#2804 {script}: the mismatch artifact must name '
                                f'the disagreement: {mismatch!r}')
                self.assertTrue('verdict roster mismatch' in run.stderr,
                                f'#2804 {script}: the refusal must name the '
                                f'mismatch: {run.stderr!r}')
                for marker in SUCCESS_LINE[script]:
                    self.assertTrue(marker not in run.stdout,
                                    f'#2804 {script}: a mismatched run must '
                                    f'not print its green line ({marker!r}): '
                                    f'{run.stdout!r}')

    def test_a_same_length_reorder_is_still_a_mismatch(self):
        # The recorded roster keeps every name and its length and only the
        # order changes, so a re-check that compared sorted lists or sets
        # would wave it through; the entries' recorded != expected compares
        # the list in order (r3, sibling P2 / boundary P2).
        for script, driver in ROSTER_GUARDED.items():
            with self.subTest(script=script):
                run = driver(self, reordered=True)
                self.assertTrue(run.code == 1,
                                f'#2804 {script}: a run whose recorded order '
                                f'disagrees with the roster must exit 1, got '
                                f'{run.code}')
                self.assertTrue(not (run.out / 'summary.json').exists(),
                                f'#2804 {script}: a mismatched run must not publish '
                                f'summary.json')
                mismatch = json.loads((run.out / 'roster-mismatch.json').read_text())
                self.assertTrue(mismatch.get('verdict') == 'ROSTER_MISMATCH',
                                f'#2804 {script}: the mismatch artifact must carry '
                                f'the verdict: {mismatch!r}')
                expected, recorded = mismatch['expected'], mismatch['recorded']
                self.assertTrue(len(expected) == len(recorded),
                                f'#2804 {script}: the reorder keeps the roster '
                                f'length, so the artifact must show two '
                                f'same-length lists: {mismatch!r}')
                self.assertTrue(set(expected) == set(recorded),
                                f'#2804 {script}: the reorder keeps every name, '
                                f'so the artifact must show two same-name '
                                f'collections: {mismatch!r}')
                self.assertTrue(list(expected) != list(recorded),
                                f'#2804 {script}: the artifact must show the '
                                f'order disagreement: {mismatch!r}')
                self.assertTrue('verdict roster mismatch' in run.stderr,
                                f'#2804 {script}: the refusal must name the '
                                f'mismatch: {run.stderr!r}')
                for marker in SUCCESS_LINE[script]:
                    self.assertTrue(marker not in run.stdout,
                                    f'#2804 {script}: a mismatched run must '
                                    f'not print its green line ({marker!r}): '
                                    f'{run.stdout!r}')

    def test_a_drifted_roster_never_starts(self):
        for script, driver in ROSTER_GUARDED.items():
            with self.subTest(script=script):
                run = driver(self, shrink_roster=True)
                self.assertTrue(run.code == 2,
                                f'#2804 {script}: a drifted roster must refuse to '
                                f'start (2), got {run.code}')
                self.assertTrue(not run.out.exists(),
                                f'#2804 {script}: a refusal must not claim the '
                                f'evidence directory')
                self.assertTrue('drifted' in run.stderr,
                                f'#2804 {script}: the refusal must name the drift: '
                                f'{run.stderr!r}')


class ManagerRosterGuard(unittest.TestCase):
    """The roster re-check must gate every summary that reads as success."""

    def drive(self, argv, *, drop_last=False, shrink_roster=False, registry=None):
        """Run the real ``main()``; return the Drive of that run."""
        return _drive_manager(self, argv, drop_last=drop_last,
                               shrink_roster=shrink_roster, registry=registry)

    def test_full_run_publishes_only_after_passing_the_roster_recheck(self):
        run = self.drive([])
        self.assertTrue(run.code == 0,
                        f'#2309 the clean full run must exit 0, got {run.code}; '
                        f'stderr={run.stderr!r}')
        summary = json.loads((run.out / 'summary.json').read_text())
        rows = summary['controls']
        self.assertTrue(len(rows) == FULL_ROSTER,
                        f'#2309 the full run must record {FULL_ROSTER} rows, got '
                        f'{len(rows)}')
        self.assertTrue(all(row['status'] == 'KILLED' for row in rows),
                        '#2309 every stubbed-kill row must be recorded KILLED')
        selected = summary.get('selected')
        self.assertTrue(selected == list(run.module.EXPECTED_NAMES),
                        '#2309 a full run must record the whole roster as selected')
        self.assertTrue(not (run.out / 'roster-mismatch.json').exists(),
                        '#2309 a matching roster must not leave roster-mismatch.json')
        for marker in SUCCESS_LINE[MANAGER_SCRIPT]:
            self.assertIn(marker, run.stdout,
                          '#2309 the green line must follow the re-check '
                          f'({marker!r})')
        # Every write to a shared product source must go through the atomic
        # writer -- a truncating write_text/write_bytes mutant publish or
        # restore is the review's SIGKILL half-file hazard. The stub records
        # every call: a clean full run makes exactly one mutant write and one
        # restore per case, so a reintroduced in-place write drops the count.
        self.assertTrue(len(run.writes) == 2 * FULL_ROSTER,
                        f'#2309 each case must publish its mutant and restore '
                        f'through the atomic writer, got {len(run.writes)} calls')
        self.assertTrue(all(path.is_relative_to(run.module.root)
                            for path, _data in run.writes),
                        '#2309 the atomic writes must target the guarded tree only')
        # Payload roles, not just counts (DS r3 sibling #3, fix-r4): per guarded
        # source the atomic writes must alternate publish (mutant bytes)
        # then restore (pristine bytes) -- a swapped pair publishes nothing
        # and leaves a mutant on the tree, which the two asserts above
        # cannot see. Manager-gateway has no window-external backstop, so
        # the sequence must be strictly publish-restore pairs. The stub
        # never writes, so the disk bytes stay pristine for the whole run.
        pristine = {path: path.read_bytes() for path, _data in run.writes}
        order = {}
        for path, data in run.writes:
            order.setdefault(path, []).append(data == pristine[path])
        for path, sequence in order.items():
            self.assertTrue(len(sequence) % 2 == 0
                            and sequence == [False, True] * (len(sequence) // 2),
                            f'#2309 atomic writes for {path.name} must alternate '
                            f'mutant publish then pristine restore with no '
                            f'dangling publish, got {sequence}')
        # The self-held window must hold terminate handlers for the whole
        # run, not only inside the shared guard's own restore window
        # (#2309 fix-r3, the mode-routing precedent). Judged at the window
        # entry, for every TERMINATE_SIGNALS member -- the same strong
        # shape the two new classes in this change pin (#2425).
        self.assertTrue(run.installed,
                        '#2309 main() must install its terminate handler for '
                        'every TERMINATE_SIGNALS member before the window '
                        'is self-held')

    def test_a_shrunken_run_publishes_no_green_summary(self):
        run = self.drive([], drop_last=True)
        self.assertTrue(run.code == 1,
                        f'#2309 a run that lost a row must exit 1, got {run.code}')
        self.assertTrue(not (run.out / 'summary.json').exists(),
                        '#2309 a shrunken run must not publish summary.json')
        for marker in SUCCESS_LINE[MANAGER_SCRIPT]:
            self.assertNotIn(marker, run.stdout,
                             '#2309 a shrunken run must not print its green '
                             f'line ({marker!r})')
        self.assertTrue('verdict roster mismatch' in run.stderr,
                        f'#2309 the refusal must name the mismatch: {run.stderr!r}')
        mismatch = json.loads((run.out / 'roster-mismatch.json').read_text())
        dropped = run.module.EXPECTED_NAMES[-1]
        self.assertTrue(dropped in mismatch['expected'],
                        f'#2309 the dropped name must appear in expected: {dropped}')
        self.assertTrue(dropped not in mismatch['recorded'],
                        f'#2309 the dropped name must be absent from recorded: {dropped}')

    def test_a_subset_run_records_its_selection(self):
        names = ['invalid-params-fallback', 'error-mapped-to-response']
        run = self.drive(['--only', ','.join(names)])
        self.assertTrue(run.code == 0,
                        f'#2309 the named subset must run green, got {run.code}; '
                        f'stderr={run.stderr!r}')
        summary = json.loads((run.out / 'summary.json').read_text())
        self.assertTrue(len(summary['controls']) == 2,
                        f'#2309 the subset run must record 2 rows, got '
                        f'{len(summary["controls"])}')
        selected = summary.get('selected')
        self.assertTrue(selected == names,
                        f'#2309 the green summary must record the executed set in '
                        f'table order, got {selected!r}')
        self.assertTrue(not (run.out / 'roster-mismatch.json').exists(),
                        '#2309 a complete subset must not leave roster-mismatch.json')

    def test_a_drifted_roster_never_starts(self):
        run = self.drive([], shrink_roster=True)
        self.assertTrue(run.code == 2,
                        f'#2309 a drifted roster must refuse to start (2), got {run.code}')
        self.assertTrue(not run.out.exists(),
                        '#2309 a refusal must not claim the evidence directory')
        self.assertTrue('roster drifted' in run.stderr,
                        f'#2309 the refusal must name the drift: {run.stderr!r}')

    def test_the_registry_identity_is_the_entry_itself(self):
        registry = RecordingRegistry(pending)
        run = self.drive([], registry=registry)
        self.assertTrue(run.code == 0,
                        f'#2309 the clean run must exit 0, got {run.code}; '
                        f'stderr={run.stderr!r}')
        # The identity key must be the entry's own path, never sys.argv[0]:
        # this driver launches the entry through a synthetic argv, and the
        # registry only resolves identities inside the fixtures tree.
        self.assertTrue(registry.calls == [ENTRY.resolve()],
                        f'#2309 for_script must receive the entry path, got '
                        f'{registry.calls!r}')

    def test_an_unusable_registry_refuses_before_claiming_the_directory(self):
        run = self.drive([], registry=UnusableRegistry())
        self.assertTrue(run.code == 2,
                        f'#2309 an unusable registry is a refusal to start (2), '
                        f'got {run.code}')
        self.assertTrue(not run.out.exists(),
                        '#2309 the refusal must not claim the evidence directory')
        self.assertTrue('registry is not usable' in run.stderr,
                        f'#2309 the refusal must name the registry on stderr: '
                        f'{run.stderr!r}')


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


class ModeRoutingGuard(unittest.TestCase):
    """#2743: mode-routing's guarded window keeps the #2425 two mirror pins.

    ``mode-routing/negative_controls.py`` already publishes and restores its
    Rust mutants through ``write_source_atomic`` and installs its terminate
    handlers before the self-held window -- but only same-json-surfaces
    (frozen ``test_safety.py``, #2425) and manager-gateway (this file,
    #2309 fix-r3) had a machine check that would go red on a regression.
    The drive mirrors ``ManagerRosterGuard``: the real ``main()`` runs with
    the cargo trio (slot gate, ``run_owned``, the pass/kill judges) and the
    snapshot window stubbed, so the real anchor walk, roster check, publish
    and verdict aggregation all execute.
    """

    def drive(self):
        """Run the real ``main()``; return (module, code, out, stdout, stderr, writes, installed)."""
        module = load_entry('mode-routing/negative_controls.py')
        base = Path(tempfile.mkdtemp(prefix='negctl-2743-mode-'))
        self.addCleanup(shutil.rmtree, base, ignore_errors=True)
        out = base / 'out'
        writes = []

        def fake_write_atomic(root, path, data, *, mode=None):
            writes.append((Path(path), bytes(data)))

        def fake_slot(script, **kwargs):
            return subprocess.CompletedProcess([], 0, '', '')

        def fake_run_owned(command, **kwargs):
            return subprocess.CompletedProcess(
                command, 0,
                'test mode_config_routes_the_native_entry ... ok\n'
                'test result: ok. 1 passed; 0 failed;\n', '')

        # main() installs its terminate handlers on this very process; the
        # test interpreter must keep its own dispositions afterwards.
        saved_handlers = {name: signal.getsignal(getattr(signal, name))
                          for name in TERMINATE_SIGNALS if hasattr(signal, name)}
        window_entry = {}
        window_exit = {}

        @contextlib.contextmanager
        def recording_window(root, paths, **kwargs):
            # #2425's strong shape (frozen test_safety.py:2862-2900, DS
            # concurrency #1/#2): record the live dispositions when the
            # window opens. A late install -- after the window entry --
            # leaves the batch unprotected while a post-return sample would
            # still see the handler; and every TERMINATE_SIGNALS member must
            # be bound, not just SIGTERM (the SIGTERM-only shape was judged
            # P1 on the frozen side once already). Sample again when the
            # window closes (DS r5 sibling#1 / boundary#1): a mid-window
            # restore to the default dispositions must stay red.
            window_entry.update({name: signal.getsignal(getattr(signal, name))
                                 for name in TERMINATE_SIGNALS
                                 if hasattr(signal, name)})
            try:
                yield {path: path.read_bytes() for path in paths}
            finally:
                window_exit.update({name: signal.getsignal(getattr(signal, name))
                                    for name in TERMINATE_SIGNALS
                                    if hasattr(signal, name)})

        installed = False
        stdout, stderr = io.StringIO(), io.StringIO()
        wrapper = base / 'wrapper-launch.py'
        environment = {**os.environ,
                       'CARGO_TARGET_DIR': str(base / 'cargo-target'),
                       'LANG': 'en_US.UTF-8'}
        try:
            with patch.dict(os.environ, environment), \
                 patch.object(module, 'mutating_sources', recording_window), \
                 patch.object(module, 'write_source_atomic', fake_write_atomic), \
                 patch.object(module, 'wait_for_slot', fake_slot), \
                 patch.object(module, 'run_owned', fake_run_owned), \
                 patch.object(module, 'passed', lambda result, test: True), \
                 patch.object(module, 'classify',
                              lambda result, case: ('KILLED',
                                                    'bound test failed at its assertion')), \
                 patch('sys.argv', [str(wrapper), str(out)]), \
                 contextlib.redirect_stdout(stdout), \
                 contextlib.redirect_stderr(stderr):
                code = module.main()
            # Judged from the window-entry AND window-exit recordings
            # above, before the restore below puts the interpreter's own
            # dispositions back: bound when the batch starts and still
            # bound when the window closes.
            installed = bool(window_entry) and bool(window_exit) and all(
                handler is module.terminate_cleanly
                for handler in window_entry.values()) and all(
                handler is module.terminate_cleanly
                for handler in window_exit.values())
        finally:
            for name, handler in saved_handlers.items():
                # getsignal returns None when a non-Python handler held the
                # signal; signal.signal refuses None (frozen side fixed this
                # in test_safety.py:2834-2840; DS concurrency #3).
                signal.signal(getattr(signal, name),
                              handler if handler is not None else signal.SIG_DFL)
        return module, code, out, stdout.getvalue(), stderr.getvalue(), writes, installed

    def test_full_run_publishes_and_restores_through_one_rename(self):
        module, code, out, stdout, stderr, writes, installed = self.drive()
        self.assertTrue(code == 0,
                        f'#2743 the clean full run must exit 0, got {code}; '
                        f'stderr={stderr!r}')
        summary = json.loads((out / 'summary.json').read_text())
        rows = summary['controls']
        # Denominator pinned to the source literal, not derived from
        # module.EXPECTED_NAMES (DS r8 + MM r8 sibling P1, hit independently
        # by both families -- #1666 family): deleting a mutations row
        # together with its EXPECTED_NAMES entry would otherwise shrink
        # every count below while the run publishes a (N-1)/(N-1) PASS.
        self.assertTrue(len(module.EXPECTED_NAMES) == MODE_ROSTER,
                        '#2743 the mode-routing roster must carry its '
                        'literal 10 names; truncating or emptying '
                        'EXPECTED_NAMES must fail here instead of '
                        'shrinking the denominator')
        self.assertTrue(len(rows) == len(module.EXPECTED_NAMES),
                        f'#2743 the full run must record every row, got '
                        f'{len(rows)}')
        self.assertTrue(all(row['status'] == 'KILLED' for row in rows),
                        '#2743 every stubbed-kill row must be recorded KILLED')
        # Every write to a shared product source must go through the atomic
        # writer -- a truncating write_text/write_bytes mutant publish or
        # restore is the #1903 SIGKILL half-file hazard #2425 pinned. The
        # stub records every call: a clean full run makes exactly one mutant
        # write and one restore per case, so a reintroduced in-place write
        # drops the count (the mode-routing mirror of the #2309 fix-r3 leg).
        self.assertTrue(len(writes) == 2 * len(module.EXPECTED_NAMES),
                        f'#2743 each case must publish its mutant and restore '
                        f'through the atomic writer, got {len(writes)} calls')
        self.assertTrue(all(path.is_relative_to(module.ROOT)
                            for path, _data in writes),
                        '#2743 the atomic writes must target the guarded tree only')
        # Payload roles, not just counts (DS concurrency #4): per guarded
        # source the atomic writes must alternate publish (mutant bytes) then
        # restore (pristine bytes) -- a swapped pair publishes nothing and
        # restores a mutant, which the count above cannot see. The stub
        # never writes, so the disk bytes stay pristine for the whole run.
        pristine = {path: path.read_bytes() for path, _data in writes}
        order = {}
        for path, data in writes:
            order.setdefault(path, []).append(data == pristine[path])
        for path, sequence in order.items():
            expected = ([False, True] * (len(sequence) // 2)
                        + ([True] if len(sequence) % 2 else []))
            self.assertTrue(sequence == expected,
                            f'#2743 atomic writes for {path.name} must alternate '
                            f'mutant publish then pristine restore, got {sequence}')
        # The self-held window must hold terminate handlers for the whole
        # run (#2425's install-and-bind pin; the handler bodies themselves
        # are pinned by the shared restore window's own defer contract).
        # Judged at both window edges, for every TERMINATE_SIGNALS member --
        # a late install, a dropped SIGHUP/SIGQUIT line, or a mid-window
        # restore to the default dispositions stays red (DS r6 P1: the exit
        # sample the third drive recorded was dead code until this fix).
        self.assertTrue(installed,
                        '#2743 main() must install its terminate handler for '
                        'every TERMINATE_SIGNALS member for the whole window')


class SessionDomainCleanupGuard(unittest.TestCase):
    """#2743: session-domain-cleanup's guarded window keeps the two pins.

    The fixture is the family's *origin* for both patterns (the
    ``terminate_cleanly`` mirror chain in specs/negative-controls/README.md
    starts at its #892 r23; the lease budget same-json-surfaces mirrors from
    it), yet its own publish/restore were still truncating writes -- the
    #1903 hazard, in the one guarded runner that mutates frozen
    ``scripts/gateway-parity`` bytes while holding the parity lease. #2743
    switched those three sites to ``write_source_atomic``; this class pins
    the result with the same stubbed-executor drive (``run_check`` children,
    the summary reader, the kill judge and the snapshot window are stubbed;
    the real anchor walk, budget check, digest bookkeeping, restored
    positive aggregation and every publish run for real). No cargo, no
    lease, no tree mutation.
    """

    def drive(self):
        """Run the real ``main()``; return (module, code, out, stdout, stderr, writes, installed)."""
        from types import SimpleNamespace
        module = load_entry('session-domain-cleanup/negative_controls.py')
        base = Path(tempfile.mkdtemp(prefix='negctl-2743-session-'))
        self.addCleanup(shutil.rmtree, base, ignore_errors=True)
        out = base / 'out'
        writes = []

        def fake_write_atomic(root, path, data, *, mode=None):
            writes.append((Path(path), bytes(data)))

        restored = SimpleNamespace(returncode=0, stdout='', stderr='')
        summary = {'status': 'PASS', 'results': [
            {'mode': 'wiring', 'passed': True, 'status': 'PASS_WITH_CLEANUP_UNVERIFIED'}]}
        # main() installs its terminate handlers on this very process; the
        # test interpreter must keep its own dispositions afterwards.
        saved_handlers = {name: signal.getsignal(getattr(signal, name))
                          for name in TERMINATE_SIGNALS if hasattr(signal, name)}
        window_entry = {}
        window_exit = {}

        @contextlib.contextmanager
        def recording_window(root, paths, **kwargs):
            # #2425's strong shape (frozen test_safety.py:2862-2900, DS
            # concurrency #1/#2): record the live dispositions when the
            # window opens, for every TERMINATE_SIGNALS member -- not a
            # post-return SIGTERM-only sample (both weaker shapes were
            # judged insufficient on the frozen side already). Sample again
            # when the window closes (DS r5 sibling#1 / boundary#1): a
            # mid-window restore to the default dispositions must stay red.
            window_entry.update({name: signal.getsignal(getattr(signal, name))
                                 for name in TERMINATE_SIGNALS
                                 if hasattr(signal, name)})
            try:
                yield {path: path.read_bytes() for path in paths}
            finally:
                window_exit.update({name: signal.getsignal(getattr(signal, name))
                                    for name in TERMINATE_SIGNALS
                                    if hasattr(signal, name)})

        installed = False
        stdout, stderr = io.StringIO(), io.StringIO()
        try:
            with patch.object(module, 'mutating_sources', recording_window), \
                    patch.object(module, 'write_source_atomic', fake_write_atomic), \
                    patch.object(module, 'pgrep_launcher', return_value=''), \
                    patch.object(module, 'run_check', return_value=restored), \
                    patch.object(module, 'read_summary', return_value=(summary, None)), \
                    patch.object(module, 'classify',
                                 lambda name, judge_spec, result, case:
                                 ('KILLED', 'stubbed judge for the window pin')), \
                    patch.object(module.sys, 'argv',
                                 ['negative_controls.py', '--output', str(out)]), \
                    patch.object(module.sys, 'platform', 'darwin'), \
                    contextlib.redirect_stdout(stdout), \
                    contextlib.redirect_stderr(stderr):
                code = module.main()
            installed = bool(window_entry) and bool(window_exit) and all(
                handler is module.terminate_cleanly
                for handler in window_entry.values()) and all(
                handler is module.terminate_cleanly
                for handler in window_exit.values())
        finally:
            for name, handler in saved_handlers.items():
                # getsignal returns None when a non-Python handler held the
                # signal; signal.signal refuses None (frozen side fixed this
                # in test_safety.py:2834-2840; DS concurrency #3).
                signal.signal(getattr(signal, name),
                              handler if handler is not None else signal.SIG_DFL)
        return module, code, out, stdout.getvalue(), stderr.getvalue(), writes, installed

    def test_full_run_publishes_and_restores_through_one_rename(self):
        module, code, out, stdout, stderr, writes, installed = self.drive()
        self.assertTrue(code == 0,
                        f'#2743 the clean full run must exit 0, got {code}; '
                        f'stdout tail={stdout[-400:]!r}; stderr={stderr!r}')
        report = json.loads((out / 'summary.json').read_text())
        # Denominator pinned to the source literal, not derived from
        # module.MUTATIONS (MM boundary P1, #1666 family): an emptied or
        # row-deleted table would otherwise make every assertion below
        # vacuously green while the run publishes a 0/0 PASS.
        names = [row[0] for row in module.MUTATIONS]
        self.assertTrue(len(set(names)) == len(names) == 331,
                        '#2743 the session-domain table must carry its '
                        'literal 331 distinct rows; truncating, emptying, '
                        'or duplicating MUTATIONS must fail here instead '
                        'of shrinking or faking the denominator')
        self.assertTrue(len(report['mutations']) > 0,
                        '#2743 the full run must record a non-empty batch')
        selected = [row['mutation'] for row in report['mutations']]
        self.assertTrue(selected == [row[0] for row in module.MUTATIONS],
                        '#2743 the full run must record the whole table in order')
        self.assertTrue(all(row['verdict'] == 'KILLED' for row in report['mutations']),
                        '#2743 every stubbed-kill row must be recorded KILLED')
        self.assertTrue(report['restoredPass'],
                        '#2743 the restored positive must be recorded separately')
        # Every write to a guarded source must go through the atomic writer:
        # one mutant publish and one per-case restore per mutation, plus the
        # window-exit backstop's one rewrite per guarded source -- the same
        # #1903 SIGKILL half-file hazard #2425 pinned, at its worst site
        # (frozen scripts/gateway-parity bytes under the parity lease).
        expected_sources = sorted({module.COMMON_SOURCE,
                                    *(Path(row[1]) for row in module.MUTATIONS)},
                                   key=str)
        self.assertTrue(len(writes)
                        == 2 * len(module.MUTATIONS) + len(expected_sources),
                        f'#2743 each case must publish and restore through the '
                        f'atomic writer plus one backstop per guarded source, '
                        f'got {len(writes)} calls for {len(module.MUTATIONS)} '
                        f'mutations over {len(expected_sources)} sources')
        self.assertTrue(all(path.is_relative_to(module.ROOT)
                            for path, _data in writes),
                        '#2743 the atomic writes must target the guarded tree only')
        self.assertTrue(all(path in expected_sources for path, _data in writes),
                        '#2743 the atomic writes must target guarded sources only')
        # Payload roles, not just counts (DS concurrency #4): per guarded
        # source the writes must alternate publish (mutant bytes) then
        # restore (pristine bytes), with the window-exit backstop's one
        # pristine rewrite allowed at the tail. A swapped pair publishes
        # nothing and restores a mutant, which the count above cannot see.
        pristine = {path: path.read_bytes() for path, _data in writes}
        order = {}
        for path, data in writes:
            order.setdefault(path, []).append(data == pristine[path])
        for path, sequence in order.items():
            expected = ([False, True] * (len(sequence) // 2)
                        + ([True] if len(sequence) % 2 else []))
            self.assertTrue(sequence == expected,
                            f'#2743 atomic writes for {path.name} must alternate '
                            f'mutant publish then pristine restore (plus one '
                            f'backstop), got {sequence}')
        # The batch loop must run under terminate handlers: an interrupted
        # batch unwinds through the finally blocks instead of leaving a
        # mutant behind under the parity lease (#892 r23 origin, #2425 pin).
        # Judged at both window edges, for every TERMINATE_SIGNALS member --
        # a late install, a dropped SIGHUP/SIGQUIT line, or a mid-window
        # restore to the default dispositions stays red (DS r6 P1: the exit
        # sample the third drive recorded was dead code until this fix).
        self.assertTrue(installed,
                        '#2743 main() must install its terminate handler for '
                        'every TERMINATE_SIGNALS member for the whole window')


def _probe_bound(bound, *, failure=None, skip=False, echo=None):
    """A real ``unittest`` transcript for ``bound``: green, failing or skipped.

    The bound name is mounted on a real TestCase through a probe class whose
    ``__module__`` is ``__main__``, then run by a real TextTestRunner at
    verbosity 2 -- so the canonical #991/#1060 judges below are exercised
    against genuine transcripts (both description layouts, 70-char
    separators, the ``Ran``/``OK`` tails), never a hand-crafted look-alike.
    ``failure`` raises the bound assertion, ``skip`` produces the green
    skip-shaped log, and ``echo`` prints the marker to stdout *outside* the
    bound assertion block while the test still fails elsewhere.
    """
    owner, method = bound.rsplit('.', 1)

    def probe(self):
        if echo is not None:
            print(echo)
        if skip:
            self.skipTest('bound probe skipped')
        if failure is not None:
            raise self.failureException(failure)

    probe.__name__ = method
    holder = type(owner, (unittest.TestCase,),
                  {'__module__': '__main__', method: probe})
    stream = io.StringIO()
    with contextlib.redirect_stdout(stream):
        result = unittest.TextTestRunner(stream=stream, verbosity=2).run(
            unittest.TestSuite([holder(method)]))
    code = 0 if result.wasSuccessful() else 1
    return subprocess.CompletedProcess([], code, stream.getvalue(), '')


class CloseOrderReaderRosterGuard(unittest.TestCase):
    """#3043 fix-r4: the close-order-reader table's own three guards.

    The fourth-cell table gained its startup roster check in fix-r2
    (``negative_selection.registered``) but had no second denominator, no
    test case for the refusal, and judge/publish shapes that had drifted
    from the family canonicals: ``passed``/``classify`` were local
    approximations of the #991/#1060 judges (a skip-shaped green log passed
    them; a marker echoed in stdout over an unrelated failure block was a
    KILL), and the mutant publish/restore were truncating
    ``write_text``/``write_bytes`` (#1903). fix-r4 aligned the judges
    verbatim and moved the writes to ``write_source_atomic``; this class
    drives the real ``main()`` with only the executor layer stubbed (the
    snapshot window, the bound-test subprocess and the atomic writer) and
    the real roster check, anchor walk, judges and verdict aggregation
    running, so each guard has a machine check that goes red on a
    regression.
    """

    def drive(self, *, shrunk=False):
        """Run the real ``main()``; return (module, code, out, stdout, stderr, writes).

        ``shrunk`` deletes the last ROWS row together with its EXPECTED_NAMES
        entry before main() starts -- the fix-r4 P1 escape shape: both sides
        of the registered() mirror shrink together.
        """
        module = load_entry('close-order-reader-failclosed/negative_controls.py')
        base = Path(tempfile.mkdtemp(prefix='negctl-3043-cor-'))
        self.addCleanup(shutil.rmtree, base, ignore_errors=True)
        out = base / 'out'
        writes = []
        calls = []

        def fake_write_atomic(root, path, data, *, mode=None):
            writes.append((Path(path), bytes(data)))

        def fake_run_bound(env):
            # Three calls per row: the positive control, the mutant run
            # (which must fail at the row's own #3043 assertion) and the
            # restored positive control.
            index = len(calls)
            calls.append(index)
            row = module.ROWS[index // 3]
            if index % 3 == 1:
                return _probe_bound(module.BOUND, failure=row[3])
            return _probe_bound(module.BOUND)

        with contextlib.ExitStack() as stack:
            stack.enter_context(patch.object(module, 'mutating_sources',
                                             stubbed_window))
            stack.enter_context(patch.object(module, 'write_source_atomic',
                                             fake_write_atomic))
            stack.enter_context(patch.object(module, 'run_bound',
                                             fake_run_bound))
            stack.enter_context(patch.object(module.sys, 'argv',
                                             ['negative_controls.py',
                                              '--output', str(out)]))
            if shrunk:
                stack.enter_context(patch.object(module, 'ROWS',
                                                 module.ROWS[:-1]))
                stack.enter_context(patch.object(module, 'EXPECTED_NAMES',
                                                 module.EXPECTED_NAMES[:-1]))
            stdout = stack.enter_context(contextlib.redirect_stdout(
                io.StringIO()))
            stderr = stack.enter_context(contextlib.redirect_stderr(
                io.StringIO()))
            code = _call_main(module)
        return (module, code, out, stdout.getvalue(), stderr.getvalue(),
                writes)

    def test_full_run_publishes_and_restores_through_one_rename(self):
        module, code, out, stdout, stderr, writes = self.drive()
        self.assertTrue(code == 0,
                        f'#3043 the clean full run must exit 0, got {code}; '
                        f'stdout={stdout[-400:]!r}; stderr={stderr!r}')
        summary = json.loads((out / 'summary.json').read_text())
        rows = summary['controls']
        # The second denominator, pinned to this file's literal and not to
        # anything module.EXPECTED_NAMES can shrink (#3043 fix-r4 P1): the
        # same-delete escape below proves registered() alone cannot keep it.
        self.assertTrue(len(module.EXPECTED_NAMES) == CLOSE_ORDER_READER_ROSTER,
                        '#3043 the close-order-reader roster must carry its '
                        'literal 6 names; truncating or emptying '
                        'EXPECTED_NAMES must fail here instead of shrinking '
                        'the denominator')
        self.assertTrue([row['mutation'] for row in rows]
                        == list(module.EXPECTED_NAMES),
                        f'#3043 the full run must record the whole roster in '
                        f'order, got {[row["mutation"] for row in rows]}')
        self.assertTrue(all(row['status'] == 'KILLED' for row in rows),
                        '#3043 every probed-kill row must be recorded KILLED '
                        'by the canonical judges')
        self.assertTrue(summary['counts'] == {'KILLED': CLOSE_ORDER_READER_ROSTER,
                                              'SURVIVED': 0, 'INVALID': 0},
                        f'#3043 the clean full run must count 6/0/0, got '
                        f'{summary["counts"]}')
        # Every write to the guarded source must go through the atomic
        # writer (fix-r4 a5): one mutant publish and one pristine restore
        # per row -- exactly 2N writes, all targeting the runner, each
        # publish carrying its own row's mutant bytes. A reintroduced
        # truncating write_text/write_bytes drops out of this record
        # entirely (#1903, the #3088 fix-r6 same-family shape).
        pristine = module.RUNNER.read_bytes()
        text = pristine.decode('utf-8')
        self.assertTrue(len(writes) == 2 * CLOSE_ORDER_READER_ROSTER,
                        f'#3043 each row must publish its mutant and restore '
                        f'through the atomic writer, got {len(writes)} calls')
        for index, (_name, before, after, _message) in enumerate(module.ROWS):
            mutant = text.replace(before, after, 1).encode('utf-8')
            self.assertTrue(writes[2 * index] == (module.RUNNER, mutant)
                           and writes[2 * index + 1] == (module.RUNNER,
                                                         pristine),
                           f'#3043 row {index} must publish its own mutant '
                           f'bytes then restore the pristine runner through '
                           f'one rename each, got '
                           f'{writes[2 * index][1][:60]!r} / '
                           f'{writes[2 * index + 1][1][:60]!r}')

    def test_a_drifted_roster_refuses_before_claiming_any_evidence(self):
        # fix-r4 a1: the table-side startup refusal had no test case (the
        # harness side had one since fix-r2). Dropping the last ROWS row
        # while EXPECTED_NAMES keeps all 6 names must refuse at rc 2 with
        # the roster diagnostic, claim no evidence directory and touch no
        # tree byte.
        module = load_entry('close-order-reader-failclosed/negative_controls.py')
        base = Path(tempfile.mkdtemp(prefix='negctl-3043-cor-'))
        self.addCleanup(shutil.rmtree, base, ignore_errors=True)
        out = base / 'out'
        before = module.RUNNER.read_bytes()
        stdout, stderr = io.StringIO(), io.StringIO()
        with patch.object(module, 'ROWS', module.ROWS[:-1]), \
                patch.object(module.sys, 'argv',
                             ['negative_controls.py', '--output', str(out)]), \
                contextlib.redirect_stdout(stdout), \
                contextlib.redirect_stderr(stderr):
            code = _call_main(module)
        self.assertTrue(code == 2,
                        f'#3043 a drifted roster must refuse at rc 2, '
                        f'got {code}; stderr={stderr.getvalue()!r}')
        self.assertTrue('INVALID: #3043 paired-mutation roster'
                        in stderr.getvalue(),
                        f'#3043 the refusal must carry the roster '
                        f'diagnostic, got {stderr.getvalue()!r}')
        self.assertTrue('control roster drifted'
                        in stderr.getvalue(),
                        '#3043 the refusal must name the drift, not a '
                        'generic refusal')
        self.assertTrue(not out.exists(),
                        '#3043 the refusal must fire before the evidence '
                        'directory is claimed')
        self.assertTrue(module.RUNNER.read_bytes() == before,
                        '#3043 the refusal must not touch the guarded tree')

    def test_the_same_delete_escape_publishes_a_shrunk_pass(self):
        # fix-r4 P1, the escape itself: delete the last ROWS row together
        # with its EXPECTED_NAMES entry and registered() sees 5 == 5 -- the
        # run publishes a 5/5 all-KILLED summary and exits 0. This test
        # documents that only the CLOSE_ORDER_READER_ROSTER literal in this
        # file (asserted by the full-run test above) can refuse the shrunk
        # run; reverting this drive to the unshrunk table makes the
        # counts mismatch the literal instead.
        module, code, out, stdout, stderr, writes = self.drive(shrunk=True)
        self.assertTrue(code == 0,
                        f'#3043 the same-delete escape must still be '
                        f'demonstrable: registered() alone lets the shrunk '
                        f'run finish green, got {code}; stderr='
                        f'{stderr!r}')
        summary = json.loads((out / 'summary.json').read_text())
        rows = summary['controls']
        self.assertTrue(len(rows) == CLOSE_ORDER_READER_ROSTER - 1
                        and all(row['status'] == 'KILLED' for row in rows),
                        f'#3043 the escaped run must record the shrunk 5/5 '
                        f'KILLED, got {len(rows)} rows')
        self.assertTrue(len(writes) == 2 * (CLOSE_ORDER_READER_ROSTER - 1),
                        f'#3043 the escaped run must still publish and '
                        f'restore every row it ran, got {len(writes)} calls')
        self.assertTrue(len(module.EXPECTED_NAMES) == CLOSE_ORDER_READER_ROSTER,
                        '#3043 the real roster must still carry its literal 6 '
                        'names -- the full-run test above fails on this '
                        'literal exactly when this escape reaches the source')

    def test_the_pass_judge_rejects_a_skip_shaped_green_log(self):
        # fix-r4 a4, the drift the local judge had: a rc-0 "Ran 1 test +
        # \nOK" log that skipped the test passed the old passed(); the
        # canonical judge must refuse it -- and still accept the real
        # green transcript.
        module = load_entry('close-order-reader-failclosed/negative_controls.py')
        skipped = _probe_bound(module.BOUND, skip=True)
        self.assertTrue(not module.passed(skipped, module.BOUND),
                        '#3043 a skip-shaped green log is not a pass: the '
                        'bound test never ran its assertions')
        green = _probe_bound(module.BOUND)
        self.assertTrue(module.passed(green, module.BOUND),
                        '#3043 the canonical judge must still accept the '
                        'real green transcript of the bound test')

    def test_the_kill_judge_requires_the_bound_tests_own_assertion_block(self):
        # fix-r4 a4, the other drift: the old classify() called any log
        # carrying the marker, a "FAIL:" line and any AssertionError a
        # KILL, so the marker echoed to stdout over an unrelated failure
        # block was judged KILLED. The canonical judge must call that
        # INVALID -- and still KILL the real at-assertion failure.
        module = load_entry('close-order-reader-failclosed/negative_controls.py')
        message = module.ROWS[0][3]
        echoed = _probe_bound(module.BOUND, failure='unrelated', echo=message)
        status, _reason = module.classify(echoed, module.BOUND, message)
        self.assertTrue(status == 'INVALID',
                        f'#3043 the marker echoed outside the bound test\'s '
                        f'own AssertionError block is not a kill, got '
                        f'{status}')
        killed = _probe_bound(module.BOUND, failure=message)
        status, _reason = module.classify(killed, module.BOUND, message)
        self.assertTrue(status == 'KILLED',
                        f'#3043 the marker inside the bound test\'s own '
                        f'AssertionError block must stay a kill, got '
                        f'{status}')


# The shared-runner four-way copy tests (fix-r6) run staged copies of the
# real driver through a boot script that pre-inserts the real fixtures
# directory on sys.path, so ``negative_process`` / ``negative_selection``
# still resolve while every byte of the driver under test comes from the
# staged copy. Same shape as the sdk-signals fix-r5 ROSTER_BOOT_SOURCE.
SHARED_RUNNER_BOOT_SOURCE = """\
import sys
copy = sys.argv[1]
sys.argv = ['negative_controls.py', *sys.argv[2:]]
sys.path.insert(0, __REAL_FIXTURES__)
import runpy
runpy.run_path(copy, run_name='__main__')
"""
# String anchors into the shared-runner source: the first table row's name
# (8-space indent inside its tuple), the second row's name, and the same
# name as it appears in EXPECTED_NAMES (4-space indent). One occurrence
# each, so a replace is surgical and a drift in the fixture turns these
# count assertions red instead of mutating the wrong line.
_SHARED_RUNNER_TABLE_ROW = "\n        'runner-baseexception-narrowed',\n"
_SHARED_RUNNER_TABLE_ROW_MOVED = "\n        'runner-publish-before-verify',\n"
# The row that must survive a first-row deletion: here the second row
# doubles as the reorder partner and the surviving-next witness.
_SHARED_RUNNER_TABLE_ROW_NEXT = _SHARED_RUNNER_TABLE_ROW_MOVED
_SHARED_RUNNER_ROSTER_ENTRY = "\n    'runner-baseexception-narrowed',\n"

# fix-r11 (P2, DS sibling#2): the pin fixture's LOCK_HOLDER_SOURCE shape
# (sdk-signals-close-order-pin/test_negative_controls.py) -- a real
# subprocess holding the repo lock through the real ``tree_lock``,
# publishing its ready file atomically, then sleeping. Replicated here (not
# imported) per the family's cross-file shape convention: importing one
# discovery-visible test module from another would couple their startup.
_REPO_LOCK_HOLDER_SOURCE = (
    'import sys, time\n'
    'from pathlib import Path\n'
    'sys.path.insert(0, sys.argv[1])\n'
    'from negative_process import tree_lock\n'
    'with tree_lock(sys.argv[2], timeout=10):\n'
    '    Path(sys.argv[3] + ".tmp").write_text("held")\n'
    '    Path(sys.argv[3] + ".tmp").replace(sys.argv[3])\n'
    '    time.sleep(30)\n'
)


def _kill_repo_lock_holder(holder):
    # Group kill, not a bare kill(): the holder runs in its own session
    # (the issue-2495 cleanup shape). A vanished group is not an error.
    if holder.poll() is None:
        try:
            os.killpg(os.getpgid(holder.pid), signal.SIGKILL)
        except ProcessLookupError:
            pass
    holder.wait(timeout=30)
    # The holder's stdout/stderr pipes (captured for death diagnosis, the
    # pin's fix-r11 concurrency#3 shape) are never read in the normal path.
    for stream in (holder.stdout, holder.stderr):
        if stream is not None:
            stream.close()


def _spawn_repo_lock_holder(testcase):
    """A sibling subprocess holding REPO_ROOT/.negative-source.lock (fix-r11).

    Returns the holder once its ready file proves the lock is held. A
    holder that dies before publishing is a premise fact, not a setup bug
    (the pin's fix-r11 concurrency#3 shape): contended lock -> named skip,
    any other death -> fail closed with the holder's stderr.
    """
    base = Path(tempfile.mkdtemp(prefix='pdrs-3043-roster-repolock-'))
    testcase.addCleanup(shutil.rmtree, base, ignore_errors=True)
    ready = base / 'held'
    holder = subprocess.Popen(
        [sys.executable, '-I', '-c', _REPO_LOCK_HOLDER_SOURCE,
         str(FIXTURES), str(REPO_ROOT), str(ready)],
        start_new_session=True, stdout=subprocess.PIPE,
        stderr=subprocess.PIPE, text=True)
    testcase.addCleanup(_kill_repo_lock_holder, holder)
    deadline = time.monotonic() + 30
    while not ready.exists():
        if holder.poll() is not None:
            _out, holder_error = holder.communicate(timeout=30)
            if 'TimeoutError' in holder_error:
                testcase.skipTest(
                    '#3043 the repo lock is already held by a sibling '
                    'negative-control round; this test\'s own holder '
                    'cannot establish its premise -- rerun when idle '
                    f'(holder stderr: {holder_error!r})')
            testcase.fail('#3043 the repo lock holder died before '
                          f'acquiring the repo lock: {holder_error!r}')
        testcase.assertLess(time.monotonic(), deadline,
                            'the repo lock holder never published its '
                            'ready file')
        time.sleep(0.05)
    return holder


class SharedRunnerRosterGuard(unittest.TestCase):
    """#3043 fix-r6: the shared-runner table's static roster refuses (rc 1).

    The sdk-signals fix-r5 mirror (``StaticRosterTests``): fix-r5 gave
    sdk-signals-close-order-pin ``EXPECTED_NAMES`` + ``registered()`` and the
    four-way copy tests in the same change, while the shared-runner table --
    grown to 39 rows in the same fix-r5 -- kept a bare ``MUTANT_ROWS``
    (DS r5 sibling P1, #3134). Measured pre-fix: a single-deleted row
    still printed ``38 anchors: ok``; a rename or a reorder kept every
    check green too, because nothing outside the table pinned the names.
    These tests drive subprocess copies of the real driver so the refusal
    fires through the module's own startup code path, never an import.

    This fixture is deliberately NOT registered in ``ROSTER_GUARDED``: that
    register is the *publish-time* roster-mismatch.json re-check (the scan
    test requires the ``roster-mismatch.json`` literal in the source, and a
    register row must carry the mismatch-publish kill set); shared-runner
    has the *startup* ``registered()`` refusal instead, the same family
    shape as ``CloseOrderReaderRosterGuard`` above -- a standalone class
    with its own outside-literal denominator, not a register row.
    """

    def run_copy(self, source, argv=(), with_output=False):
        """Run a staged copy of the driver in a temp tree; return (result, out).

        ``argv`` lands after the script name, so the copy's own ``main()``
        parses it exactly like a real invocation. The guarded sources the
        ``--check`` walk reads are staged next to the copy so the pristine
        direction exercises the real 39-anchor walk end to end.
        ``with_output=True`` appends ``--output <out>`` so the refusal cases
        below stay falsifiable: if a pre-run refusal ever stopped firing,
        the run would proceed past argparse and create ``out`` (DS fix-r6
        boundary P2 -- the bare ``assertFalse(out.exists())`` used to hold
        vacuously because no path ever created ``out``).
        """
        base = Path(tempfile.mkdtemp(prefix='pdrs-3043-shared-runner-roster-'))
        self.addCleanup(shutil.rmtree, base, ignore_errors=True)
        fixtures = base / ('src/pilotdeck-gateway/tests/fixtures')
        tree = fixtures / 'shared-runner-failclosed'
        tree.mkdir(parents=True)
        copy = tree / 'negative_controls.py'
        copy.write_text(source, encoding='utf-8')
        # The driver resolves SUITE/SQLITE from its own __file__ (parents[5]
        # + the fixtures suffix): stage the real guarded bytes so --check
        # walks the true 39 anchors in the temp tree. fix-r8
        # (concurrency#1): those two files are exactly the shared-runner
        # control's SUITE/SQLITE, and it keeps a mutant on disk for the
        # whole bound-test window under the tree lock -- copying without
        # the lock can land on mutant bytes and red the anchor walk below
        # (TreeLock: whoever owns the lock reads the pristine bytes
        # *after* acquisition). The lock is held only across the copy
        # window; the subprocess below runs staged bytes, unlocked.
        # fix-r9 (MM concurrency#1 ≡ DS concurrency#1, the fast_tree_lock
        # precedent at :628): the wait budget is explicit and short, not
        # TreeLock's silent 900 s default -- a sibling round holding
        # ROOT/.negative-source.lock fails this copy closed in seconds
        # with a holder-naming diagnostic instead of outlasting the CI
        # step's own timeout.
        try:
            with tree_lock(REPO_ROOT, timeout=2):
                shutil.copy2(FIXTURES / 'negative_suite.py',
                             fixtures / 'negative_suite.py')
                shared = fixtures / '_shared'
                shared.mkdir()
                shutil.copy2(FIXTURES / '_shared/negative_sqlite_header.py',
                             shared / 'negative_sqlite_header.py')
        except TimeoutError as error:
            self.fail('#3043 the guarded shared sources were busy for over '
                      '2 s: a sibling negative-control round holds '
                      f'{REPO_ROOT}/.negative-source.lock and mutant bytes '
                      f'would be staged as pristine; refusing: {error}')
        boot = base / 'boot.py'
        boot.write_text(
            SHARED_RUNNER_BOOT_SOURCE.replace('__REAL_FIXTURES__',
                                              repr(str(FIXTURES))),
            encoding='utf-8')
        out = base / 'out'
        if with_output:
            argv = (*argv, '--output', str(out))
        result = subprocess.run(
            [sys.executable, '-B', str(boot), str(copy), *argv],
            capture_output=True, text=True, timeout=60,
            env={key: value for key, value in os.environ.items()
                 if key != 'CARGO_TARGET_DIR'})
        return result, out

    def test_a_busy_repo_lock_fails_closed_within_the_budget(self):
        # fix-r11 (P2, DS sibling#2): the copy window's busy branch had no
        # paired control in this file -- the pin fixture's
        # guarded_source_window got one in fix-r10 P2#2, but run_copy's own
        # ``tree_lock(REPO_ROOT, timeout=2)`` (the same family) stayed
        # uncovered: reverting the timeout to TreeLock's 900 s default or
        # dropping the fail branch entirely keeps every test here green. A
        # sibling process holds the real repo lock, the very window must
        # refuse with the named diagnostic inside its 2 s budget, and the
        # same guarded read acquires cleanly once the holder is gone.
        source = (FIXTURES / 'shared-runner-failclosed/negative_controls.py'
                  ).read_text(encoding='utf-8')
        holder = _spawn_repo_lock_holder(self)
        try:
            started = time.monotonic()
            with self.assertRaises(AssertionError) as raised:
                self.run_copy(source)
            elapsed = time.monotonic() - started
        finally:
            _kill_repo_lock_holder(holder)
        message = str(raised.exception)
        self.assertIn('#3043 the guarded shared sources were busy', message)
        self.assertIn('.negative-source.lock', message)
        self.assertGreaterEqual(
            elapsed, 1.0,
            'the copy window must really wait its 2 s budget out before '
            'refusing, not fail on some earlier error')
        self.assertLessEqual(
            elapsed, 5.0,
            'the refusal must land at the 2 s budget (the window must '
            'never silently outlast it toward the holder\'s sleep horizon)')
        # Positive control: once the holder is gone the very same lock
        # window acquires and the guarded bytes read cleanly.
        with tree_lock(REPO_ROOT, timeout=2):
            self.assertTrue(
                (FIXTURES / 'negative_suite.py').read_bytes()
                and (FIXTURES / '_shared/negative_sqlite_header.py'
                     ).read_bytes(),
                'the copy window must read the guarded shared sources '
                'once the holder is gone')

    def test_a_renamed_row_is_refused_without_evidence(self):
        source = (FIXTURES / 'shared-runner-failclosed/negative_controls.py'
                  ).read_text(encoding='utf-8')
        self.assertEqual(source.count(_SHARED_RUNNER_TABLE_ROW), 1)
        result, out = self.run_copy(
            source.replace(_SHARED_RUNNER_TABLE_ROW,
                           _SHARED_RUNNER_TABLE_ROW.replace('narrowed',
                                                            'narrowed-x')),
            with_output=True)
        self.assertEqual(result.returncode, 1,
                         f'#3043 a renamed row is a pre-run refusal (rc 1): '
                         f'{result.stderr!r}')
        self.assertIn('#1076 mutation roster drifted from the static inventory',
                      result.stderr)
        self.assertIn('runner-baseexception-narrowed', result.stderr)
        self.assertIn('runner-baseexception-narrowed-x', result.stderr)
        self.assertFalse(out.exists())

    def test_a_reordered_row_is_refused_without_evidence(self):
        source = (FIXTURES / 'shared-runner-failclosed/negative_controls.py'
                  ).read_text(encoding='utf-8')
        self.assertEqual(source.count(_SHARED_RUNNER_TABLE_ROW), 1)
        self.assertEqual(source.count(_SHARED_RUNNER_TABLE_ROW_MOVED), 1)
        swapped = (source
                   .replace(_SHARED_RUNNER_TABLE_ROW, '\n        "__swap__",\n')
                   .replace(_SHARED_RUNNER_TABLE_ROW_MOVED,
                            _SHARED_RUNNER_TABLE_ROW)
                   .replace('\n        "__swap__",\n',
                            _SHARED_RUNNER_TABLE_ROW_MOVED))
        result, out = self.run_copy(swapped, with_output=True)
        self.assertEqual(result.returncode, 1,
                         f'#3043 a reordered row is a pre-run refusal (rc 1): '
                         f'{result.stderr!r}')
        self.assertIn('#1076 mutation roster drifted from the static inventory',
                      result.stderr)
        self.assertFalse(out.exists())

    def test_a_row_deleted_with_its_roster_entry_escapes_registered(self):
        # The double edit registered() alone cannot see (both sides shrink
        # to 38 together) is exactly why the SHARED_RUNNER_ROSTER literal
        # stays: the pair is two-sided, and the pin below refuses this
        # direction when it reaches the source. The escape itself must stay
        # demonstrable: the shrunk copy clears the roster and dies at
        # main()'s own usage refusal instead.
        source = (FIXTURES / 'shared-runner-failclosed/negative_controls.py'
                  ).read_text(encoding='utf-8')
        self.assertEqual(source.count(_SHARED_RUNNER_TABLE_ROW), 1)
        self.assertEqual(source.count(_SHARED_RUNNER_ROSTER_ENTRY), 1)
        index = source.index(_SHARED_RUNNER_TABLE_ROW)
        # The anchor's leading newline is also the tuple-opening line's
        # terminator, so the row's own ``    (\n`` spans [index-5, index+1)
        # and the search end must be index + 1 to include it (the sdk
        # -signals geometry deletes one row too many and lets its
        # EXPECTED_TOTAL count check absorb that; shared-runner has only
        # registered(), so this deletion must be surgical: exactly one
        # row and its roster entry, both sides shrinking 39 -> 38).
        start = source.rfind('    (\n', 0, index + 1)
        end = source.index('    (\n', index + 1)
        without_row = source[:start] + source[end:]
        self.assertNotIn(_SHARED_RUNNER_TABLE_ROW, without_row)
        self.assertEqual(without_row.count(_SHARED_RUNNER_TABLE_ROW_NEXT), 1)
        result, out = self.run_copy(without_row.replace(
            _SHARED_RUNNER_ROSTER_ENTRY, ''))
        self.assertEqual(result.returncode, 2,
                         f'#3043 the same-delete escape must clear the roster '
                         f'and reach main() own refusal (rc 2): '
                         f'{result.stderr!r}')
        self.assertIn('--output is required unless --check is used',
                      result.stderr)
        # Shape guard only (DS fix-r6 boundary P2): this case deliberately
        # does NOT pass --output -- the pinned refusal is exactly the
        # missing-output usage error, so no path here creates ``out``.
        self.assertFalse(out.exists())
        # The outside literal: the real module must still carry the whole
        # 39-name pair, so the shrunk source fails this pin exactly when
        # the escape reaches it.
        module = load_entry('shared-runner-failclosed/negative_controls.py')
        self.assertEqual(len(module.MUTANT_ROWS), SHARED_RUNNER_ROSTER,
                         f'#3043 the shared-runner table must carry its '
                         f'literal {SHARED_RUNNER_ROSTER} rows, got '
                         f'{len(module.MUTANT_ROWS)}')
        self.assertEqual([row[0] for row in module.MUTANT_ROWS],
                         list(module.EXPECTED_NAMES),
                         '#3043 the real table must stay registered against '
                         'its full static roster')

    def test_a_pristine_copy_walks_the_whole_table(self):
        # The pristine width direction: the refusals above must come from
        # the roster, not the bootstrap, and the healthy copy must still
        # walk the real anchor table end to end through --check.
        result, out = self.run_copy(
            (FIXTURES / 'shared-runner-failclosed/negative_controls.py'
             ).read_text(encoding='utf-8'),
            argv=('--check',), with_output=True)
        self.assertEqual(result.returncode, 0,
                         f'#3043 the pristine copy must walk all '
                         f'{SHARED_RUNNER_ROSTER} anchors and exit 0: '
                         f'{result.stdout!r} {result.stderr!r}')
        self.assertIn(f'{SHARED_RUNNER_ROSTER} anchors: ok',
                      result.stdout)
        self.assertFalse(out.exists())


if __name__ == '__main__':
    unittest.main()
