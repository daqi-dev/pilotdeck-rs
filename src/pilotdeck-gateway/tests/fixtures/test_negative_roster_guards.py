#!/usr/bin/env python3
# anchor-selfcheck: unsupported meta-test of the roster verify-before-publish guard, drives every registered entry's main() with a stubbed cargo
"""#2804: the verify-before-publish roster check needs a denominator per entry.

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
import unittest
from unittest.mock import patch

import pending_reanchor as pending

FIXTURES = Path(__file__).resolve().parent
FULL_ROSTER = 36
TERMINATE_SIGNALS = ('SIGTERM', 'SIGHUP', 'SIGQUIT')
MANAGER_SCRIPT = 'manager-gateway/negative_controls.py'
ENTRY = FIXTURES / MANAGER_SCRIPT

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
    installed = False
    stdout, stderr = io.StringIO(), io.StringIO()
    # A launcher identity that is NOT the entry path (a wrapper or symlink
    # launch): the registry only resolves identities inside the fixtures
    # tree, so an argv[0]-based lookup must not pass for this entry.
    wrapper = base / 'wrapper-launch.py'
    try:
        with patch.dict(os.environ, _environment(base)), \
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
                if registry is not None:
                    with patch.object(module, 'pending', registry):
                        code = module.main()
                else:
                    code = module.main()
            except ValueError as error:
                code = f'uncaught ValueError: {error}'
        # Sampled before the restore below puts the interpreter's own
        # dispositions back.
        installed = signal.getsignal(signal.SIGTERM) is module.terminate_cleanly
    finally:
        for name, handler in saved_handlers.items():
            signal.signal(getattr(signal, name), handler)
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
        # The self-held window must hold terminate handlers for the whole
        # run, not only inside the shared guard's own restore window
        # (#2309 fix-r3, the mode-routing precedent).
        self.assertTrue(run.installed,
                        '#2309 main() must install its terminate handler for '
                        'SIGTERM while the window is self-held')

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


if __name__ == '__main__':
    unittest.main()
