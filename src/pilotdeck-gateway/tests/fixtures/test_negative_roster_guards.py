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
import ast
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

import negative_process
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


class _RecordingTreeLock:
    """A stubbed ``tree_lock`` for the --audit drive: records acquisition.

    ``busy`` makes every ``__enter__`` raise ``TimeoutError`` (the entry's
    documented busy-lock refusal shape, issue-2495's bounded-wait precedent).
    """

    def __init__(self, events, *, busy=False):
        self.events = events
        self.busy = busy
        self.held = False
        # #2795 fix-r3（评审 sibling#3/concurrency#2）：锁实参必须记名——
        # 锁到错误的树（root.parent）或任意超时都不许静默绿；
        # session-lease/test_lease.py:282 的 seen 同形。
        self.calls = []

    def __call__(self, *args, **kwargs):
        self.calls.append((args, kwargs))
        return self

    def __enter__(self):
        if self.busy:
            raise TimeoutError('audit drive: lock timeout (#2795 fix-r2)')
        self.events.append('lock-enter')
        self.held = True
        return self

    def __exit__(self, *_exc):
        self.held = False
        self.events.append('lock-exit')
        return False


class AuditDrive:
    """One driven ``--audit`` run of manager-gateway's real ``main()``."""

    def __init__(self, module, code, stdout, stderr, events, lock=None):
        self.module = module
        self.code = code
        self.stdout = stdout
        self.stderr = stderr
        self.events = events
        # #2795 fix-r3（评审 sibling#3/concurrency#2）：驱动器带回 fake 锁，
        # 用例可断言 tree_lock 收到的实参（仓根 + timeout=60）。
        self.lock = lock


def _drive_manager_audit(testcase, *, busy_lock=False, fail_first_read=False):
    """Run manager-gateway's real ``main()`` in ``--audit`` mode.

    Only the audit read surface's seams are stubbed: ``tree_lock`` becomes a
    recording fake (refusing on ``busy_lock``) and ``mutate`` a wrapper that
    records whether the shared lock was held at each anchor check.
    ``fail_first_read`` makes the first selected case's source read raise
    OSError. The selection, the roster check, the real anchors and the JSON
    publication all go through the entry's real code; no cargo, no tree
    mutation, no evidence directory.
    #2795 fix-r3（评审 concurrency#1 / boundary#2）: the held-lock check also
    sits on the patched ``read_text`` itself, so an entry that reads every
    source *before* entering the lock and only calls ``mutate`` inside it is
    still red; and every completed anchor check records ``('checked', name)``
    so "the audit actually checked the roster" is an assertable quantity, not
    an implied one.
    """
    module = load_entry(MANAGER_SCRIPT)
    base = Path(tempfile.mkdtemp(prefix='negctl-2795r2-'))
    testcase.addCleanup(shutil.rmtree, base, ignore_errors=True)
    events = []
    lock = _RecordingTreeLock(events, busy=busy_lock)
    real_mutate = module.mutate
    anchor_to_name = {row[2]: row[0] for row in module.cases}

    def recording_mutate(original, scope, replacements):
        # The kill target: every anchor check must happen with the shared tree
        # lock held -- an unlocked read races a sibling runner's mutant window.
        if not lock.held:
            events.append('unlocked-read')
        # #2795 fix-r3（评审 boundary#2）：核对「发生」本身可判红——每次锚点
        # 核对记行名，成功用例断言序列等于选中名册全表。
        events.append(('checked', anchor_to_name.get(replacements[0][0])))
        return real_mutate(original, scope, replacements)

    unreadable = module.cases[0][1].resolve() if fail_first_read else None
    real_read_text = pathlib.Path.read_text

    def failing_read_text(self, *args, **kwargs):
        # #2795 fix-r3（评审 concurrency#1）：持锁检查下沉到被 patch 的
        # read_text 层——只在 mutate 里查锁会把「锁外先读 texts、锁内空转
        # mutate」的形态放绿，读本身才是要钉住的读面。
        if not lock.held:
            events.append('unlocked-read')
        if unreadable is not None and self.resolve() == unreadable:
            raise OSError('audit drive: unreadable source (#2795 fix-r2)')
        return real_read_text(self, *args, **kwargs)

    stdout, stderr = io.StringIO(), io.StringIO()
    wrapper = base / 'wrapper-launch.py'
    with patch.object(module, 'tree_lock', lock), \
         patch.object(module, 'mutate', recording_mutate), \
         patch.object(pathlib.Path, 'read_text', failing_read_text), \
         patch('sys.argv', [str(wrapper), '--audit']), \
         contextlib.redirect_stdout(stdout), \
         contextlib.redirect_stderr(stderr):
        try:
            code = module.main()
        except SystemExit as error:
            code = error.code if isinstance(error.code, int) else 1
        except (TimeoutError, OSError, UnicodeDecodeError) as error:
            # A refusal must be a returned 2 with the named message, never an
            # exception escaping main() -- a bare traceback reads as rc 1 on the
            # host and carries no dedicated refusal message.
            code = f'uncaught {type(error).__name__}: {error}'
        except ValueError as error:
            code = f'uncaught ValueError: {error}'
    return AuditDrive(module, code, stdout.getvalue(), stderr.getvalue(),
                      events, lock)


def _drive_snapshot_audit(testcase, *, busy_lock=False, fail_first_read=False):
    """Run snapshot-projection's real ``main()`` in ``--audit`` mode.

    #2795 fix-r3（评审 sibling#2，B③ 配对负控）：fix-r1 给 snapshot-projection
    的 ``--audit`` 加了树锁 / 拒跑，此前没有任何机检——把 ``with tree_lock``
    删掉或把异常分支退回直冒 CI 仍绿。本驱动复用 manager 侧的
    ``_RecordingTreeLock`` / ``AuditDrive`` 形态（只换入口），同样只 stub
    读面缝隙：``tree_lock`` 换成记名 fake（``busy_lock`` 时 ``__enter__`` 抛
    TimeoutError），``fail_first_read`` 让首个源读抛 OSError。剥参、名册
    对账、真实锚点与 ``#888 anchors: N/N unique`` 发布全走入口真码；
    无 cargo、无树变异、无证据目录。
    """
    module = load_entry('snapshot-projection/negative_controls.py')
    base = Path(tempfile.mkdtemp(prefix='negctl-2795r3-'))
    testcase.addCleanup(shutil.rmtree, base, ignore_errors=True)
    events = []
    lock = _RecordingTreeLock(events, busy=busy_lock)
    unreadable = module.mutations[0][1].resolve() if fail_first_read else None
    real_read_text = pathlib.Path.read_text

    def failing_read_text(self, *args, **kwargs):
        # Same read-level pin as the manager drive: the audit's occurrence
        # counts must be computed on bytes read under the lock, so an
        # unlocked read is red even though this entry never calls mutate().
        if not lock.held:
            events.append('unlocked-read')
        if unreadable is not None and self.resolve() == unreadable:
            raise OSError('audit drive: unreadable source (#2795 fix-r3)')
        return real_read_text(self, *args, **kwargs)

    stdout, stderr = io.StringIO(), io.StringIO()
    wrapper = base / 'wrapper-launch.py'
    with patch.object(module, 'tree_lock', lock), \
         patch.object(pathlib.Path, 'read_text', failing_read_text), \
         patch('sys.argv', [str(wrapper), '--audit']), \
         contextlib.redirect_stdout(stdout), \
         contextlib.redirect_stderr(stderr):
        try:
            code = module.main()
        except SystemExit as error:
            code = error.code if isinstance(error.code, int) else 1
        except (TimeoutError, OSError, UnicodeDecodeError) as error:
            code = f'uncaught {type(error).__name__}: {error}'
        except ValueError as error:
            code = f'uncaught ValueError: {error}'
    return AuditDrive(module, code, stdout.getvalue(), stderr.getvalue(),
                      events, lock)


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


class ManagerAuditReadSurface(unittest.TestCase):
    """The ``--audit`` read surface must hold the shared tree lock and refuse.

    #2795 fix-r2 (sibling P2②): snapshot-projection's ``--audit`` grew the
    tree lock and the fail-closed read (fix-r1 条2) while this same-family
    entry kept its unlocked, uncaught loop -- a sibling mutation runner with
    a mutant between its write and restore phases made the read-only audit
    check anchors against a source it does not own, and a busy lock or an
    unreadable source surfaced as a bare traceback a host reads as rc 1.
    """

    def test_audit_reads_only_inside_the_tree_lock(self):
        run = _drive_manager_audit(self)
        self.assertTrue(run.code == 0,
                        f'#2795 the anchors-only audit must exit 0, got '
                        f'{run.code!r}; stderr={run.stderr!r}')
        rows = json.loads(run.stdout)
        self.assertTrue([row['mutation'] for row in rows]
                        == list(run.module.EXPECTED_NAMES),
                        f'#2795 the audit must publish the selected roster, '
                        f'got {run.stdout[:200]!r}')
        self.assertTrue('unlocked-read' not in run.events,
                        '#2795 every source read must happen with the '
                        'shared tree lock held (a sibling runner may have a '
                        'mutant on disk between its write and restore '
                        'phases; reading before the lock and only mutating '
                        'inside it is the same race)')
        self.assertTrue(run.events.count('lock-enter') == 1,
                        f'#2795 the audit must acquire the tree lock exactly '
                        f'once around the whole anchor loop: {run.events!r}')
        self.assertTrue(run.events[0] == 'lock-enter'
                        and run.events[-1] == 'lock-exit',
                        f'#2795 the lock must wrap every read, not be taken '
                        f'and dropped per anchor: {run.events!r}')
        # #2795 fix-r3（评审 sibling#3/concurrency#2）：锁实参同形钉死——仓根
        # 与 timeout=60，锁到 root.parent 或换超时都红。
        self.assertTrue(run.lock.calls == [((run.module.root,),
                                            {'timeout': 60})],
                        f'#2795 the audit must lock the repo root with the '
                        f'60 s bounded wait, got {run.lock.calls!r}')
        # #2795 fix-r3（评审 boundary#2）：核对「发生」可判红——checked 序列
        # 必须等于选中名册全表，循环体删成 pass 是零核对假绿。
        checked = [entry[1] for entry in run.events
                   if isinstance(entry, tuple) and entry[0] == 'checked']
        self.assertTrue(checked == [row[0] for row in run.module.cases],
                        f'#2795 the audit must check every roster row '
                        f'exactly once, got {checked!r}')

    def test_audit_refuses_when_the_tree_lock_is_busy(self):
        run = _drive_manager_audit(self, busy_lock=True)
        self.assertTrue(run.code == 2,
                        f'#2795 a busy tree lock is a refusal to start (2), '
                        f'got {run.code!r}')
        self.assertTrue('--audit tree lock or read failed' in run.stderr,
                        f'#2795 the refusal must name the audit read '
                        f'surface: {run.stderr!r}')
        self.assertTrue('TimeoutError' in run.stderr,
                        f'#2795 the refusal must name the lock wait as the '
                        f'cause: {run.stderr!r}')
        self.assertTrue('unlocked-read' not in run.events and
                        run.events == [],
                        f'#2795 a refused audit must check no anchors: '
                        f'{run.events!r}')
        # #2795 fix-r3（评审 boundary#2）：拒跑必须零核对。
        checked = [entry for entry in run.events
                   if isinstance(entry, tuple) and entry[0] == 'checked']
        self.assertTrue(checked == [],
                        f'#2795 a refused audit must record no checked '
                        f'anchors: {checked!r}')

    def test_audit_refuses_when_a_source_read_fails(self):
        run = _drive_manager_audit(self, fail_first_read=True)
        self.assertTrue(run.code == 2,
                        f'#2795 an unreadable anchor source is a refusal to '
                        f'start (2), got {run.code!r}')
        self.assertTrue('--audit tree lock or read failed' in run.stderr
                        and 'OSError' in run.stderr,
                        f'#2795 the refusal must name the audit read surface '
                        f'and the read failure: {run.stderr!r}')
        # #2795 fix-r3（评审 boundary#2）：读失败拒跑必须零核对。
        checked = [entry for entry in run.events
                   if isinstance(entry, tuple) and entry[0] == 'checked']
        self.assertTrue(checked == [],
                        f'#2795 a read-failure refusal must record no '
                        f'checked anchors: {checked!r}')


class SnapshotAuditReadSurface(unittest.TestCase):
    """snapshot-projection's ``--audit`` read surface: the paired guard.

    #2795 fix-r3 (sibling P2, B③ 配对负控): fix-r1 gave this entry's
    ``--audit`` the tree lock and the fail-closed read, but no machine check
    existed — deleting the ``with tree_lock`` or reverting the refusal to a
    traceback kept CI green. Same three pins as ``ManagerAuditReadSurface``,
    through the entry's real ``main()``.
    """

    def test_audit_reads_only_inside_the_tree_lock(self):
        run = _drive_snapshot_audit(self)
        self.assertTrue(run.code == 0,
                        f'#2795 the anchors-only audit must exit 0, got '
                        f'{run.code!r}; stderr={run.stderr!r}')
        rows = len(run.module.mutations)
        self.assertTrue(run.stdout.strip()
                        == f'#888 anchors: {rows}/{rows} unique',
                        f'#2795 the audit must publish the full anchor '
                        f'count, got {run.stdout!r}')
        self.assertTrue('unlocked-read' not in run.events,
                        '#2795 every anchor read must happen with the '
                        'shared tree lock held (a sibling runner may have '
                        'a mutant on disk between its write and restore '
                        'phases)')
        self.assertTrue(run.events.count('lock-enter') == 1
                        and run.events[0] == 'lock-enter'
                        and run.events[-1] == 'lock-exit',
                        f'#2795 the lock must wrap the whole anchor loop '
                        f'exactly once: {run.events!r}')
        # #2795 fix-r3（评审 sibling#3/concurrency#2 同形）：仓根 + 60 s。
        self.assertTrue(run.lock.calls == [((run.module.ROOT,),
                                            {'timeout': 60})],
                        f'#2795 the audit must lock the repo root with the '
                        f'60 s bounded wait, got {run.lock.calls!r}')

    def test_audit_refuses_when_the_tree_lock_is_busy(self):
        run = _drive_snapshot_audit(self, busy_lock=True)
        self.assertTrue(run.code == 2,
                        f'#2795 a busy tree lock is a refusal to start (2), '
                        f'got {run.code!r}')
        self.assertTrue('--audit tree lock or read failed' in run.stderr,
                        f'#2795 the refusal must name the audit read '
                        f'surface: {run.stderr!r}')
        self.assertTrue(run.events == [],
                        f'#2795 a refused audit must read no anchors: '
                        f'{run.events!r}')

    def test_audit_refuses_when_a_source_read_fails(self):
        run = _drive_snapshot_audit(self, fail_first_read=True)
        self.assertTrue(run.code == 2,
                        f'#2795 an unreadable anchor source is a refusal to '
                        f'start (2), got {run.code!r}')
        # This entry's refusal prints the error itself (no type name), so the
        # named surface plus the drive's own marker pin the read failure.
        self.assertTrue('--audit tree lock or read failed' in run.stderr
                        and 'unreadable source' in run.stderr,
                        f'#2795 the refusal must name the audit read surface '
                        f'and the read failure: {run.stderr!r}')


# ---------------------------------------------------------------------------
# #2795 fix-r4 (D 节)：只读锚点入口名册——「读必须发生在锁窗口内」机检。
# fix-r2/fix-r3 给个把入口的 --audit / --check-anchors 补了树锁（manager /
# snapshot 有上面的专用机检），fix-r4 把 B 节全部入口 + fix-r3 已修的 21 个
# 入口钉成一张名册：每个入口用真实 main()（或在 import 期分发的入口用真实
# 源码 exec）驱动其只读锚点模式，只有 ``tree_lock`` 和读面被记名——
# 锁外任何 ``Path.read_text`` / ``read_bytes`` 都是红。名册的登记签名是
# 拒跑消息本身（``tree lock or anchor read failed`` / ``tree lock or
# read failed`` / ``tree lock or stale read failed``），扫描双向保真：长出
# 同形防务的入口不登记就红（防下批再漏），登记的入口不存在也红。
# ---------------------------------------------------------------------------

# The refusal message every family entry prints on a busy lock or a failed
# read. Three spellings exist (fix-r4's neutral prefix, fix-r3's mode-qualified
# forms, and issue-2495's own stale-scan wording from #2590 fix-r3b); they stay
# matched until the legacy sites are migrated onto the shared device in batch
# (see the #2795 fix-r5 续跑表 in RESULT.md).
READ_SURFACE_MARKER = re.compile(
    r'''tree lock or (?:anchor read|read|stale read) failed''')

# #2795 fix-r5（simplify P2#1 连轮结构性收口）：fix-r2/r3/r4 连续四轮逐点补
# 同一防务，54 个文件手抄出三种拒跑文案——名册按文案认入口，第 4 种拼写的
# 新入口扫不出（fix-r4 自己的注释承认的机制缺口）。共享装置
# ``negative_process.anchor_read_window`` 落地后，登记签名扩成「装置名」：
# 新入口改走装置即被扫出（文案只写在装置里）；上面的旧三拼写保持兼容，
# 存量入口按续跑表分批迁移。
ANCHOR_READ_WINDOW_CALL = re.compile(r'(?<!def )anchor_read_window\(')
# A device call's literal label (every device site spells its label as a
# single-quoted literal): the busy-refusal test pins that the message on
# stderr carries the entry's own label, not just the shared spelling.
ANCHOR_READ_WINDOW_LABEL = re.compile(
    r"anchor_read_window\(\s*\w+(?:\.\w+)*,\s*'([^']+)'")


def read_surface_signature(text):
    """An entry registers either by calling the shared device or by the
    legacy family message; the scan keeps both directions honest.

    #2795 fix-r6（boundary#2）：装置调用认 import 别名形态——
    ``from negative_process import anchor_read_window as w`` + ``w(ROOT, ...)``
    既不匹配装置名正则也不匹配旧文案，此前两签名都扫不到，登记防务的
    新入口照样进不了名册（假 PASS）。AST 解析别名并认别名/属性调用点；
    解析失败的源退回字面正则（不比修复前更宽）。
    """
    if ANCHOR_READ_WINDOW_CALL.search(text) or READ_SURFACE_MARKER.search(text):
        return True
    try:
        tree = ast.parse(text)
    except SyntaxError:
        return False
    return _anchor_read_window_alias_call(tree)


def _anchor_read_window_alias_call(tree):
    """True when the AST calls the device through an import alias."""
    aliases = set()
    for node in ast.walk(tree):
        if (isinstance(node, ast.ImportFrom)
                and node.module == 'negative_process'):
            for alias in node.names:
                if alias.name == 'anchor_read_window':
                    aliases.add(alias.asname or alias.name)
    if not aliases:
        return False
    for node in ast.walk(tree):
        if isinstance(node, ast.Call) and isinstance(node.func, ast.Name) \
                and node.func.id in aliases:
            return True
    return False


# ---------------------------------------------------------------------------
# #2795 fix-r6（评审 sibling P1，boundary/concurrency 拼合角色复核）：
# 裸读 --check/--check-anchors 预检的静态扫描。名册签名只认「装置调用
# （含别名）/ 旧三拼写文案」，既无装置也无文案的检查入口是签名盲区——
# 评审按两种字面形状取样 11 站（native-cron 等），本扫描按 AST 形状收口：
# 凡检查门——「args == ['--check'] / ['--check-anchors']」字面等值比较
# （if / 条件表达式，右值单元素常量序列）、argparse 形态的
# ``if args.check:`` 属性门，或 fix-r7（boundary#1 + sibling#1）补的三种
# 真实拼写——「'--check-anchors' in sys.argv」成员测试、「args in
# (["--check"], ["--check-anchors"])」别名元组成员测试、「mode ==
# '--check-anchors'」字符串常量比较（含 BoolOp/UnaryOp 嵌套，见
# _has_check_mode_gate）——（同族：读面多经 ``mutant_switch.preflight``
# 委托进 _shared/mutant_switch.py，文件级看不见 read_text，故门形状即
# 家族形状）——且文件内无装置调用（含别名）、无旧文案、无 matrix_main
# 分发（共享 --check 的锁由 negative_suite 的装置持有）、无 tree_lock 锁
# 纪律（旧三拼写 55 站的形态），又不在续跑表豁免清单里 → 红。豁免清单 =
# legacy-migration-backlog.txt 本身（单一真相源，测试不复制清单）。已知
# 盲区（登记不修）：test_* 前缀文件、门内经本文件 helper 或跨模块 helper
# 裸读且文件另处持 tree_lock 的站（人工登记进续跑表「盲区登记」节）、
# 门经中间布尔变量拼写的站（谓词只看 If/IfExp 的 test，该拼写的 Compare
# 在 Assign 里——fix-r8 boundary#3 登记进续跑表「二（续三）」节）。
# ---------------------------------------------------------------------------
LEGACY_MIGRATION_BACKLOG = FIXTURES / 'legacy-migration-backlog.txt'

# The names a check-gate file may reference without being bare: the shared
# matrix runner holds the device window for its --check callers, and any
# tree_lock use is the legacy hand-rolled lock shape the 55-entry backlog
# section tracks. AST names only -- a comment mentioning matrix_main must
# not exempt an entry.
_COVERED_CODE_NAMES = frozenset({'matrix_main', 'tree_lock', 'anchor_read_window'})


def backlog_exempted_entries():
    """Every backlog line's first token is a fixtures-relative path.

    The backlog is the single source of truth for scan exemptions: a
    non-comment line starts with the exempted ``*.py`` path, everything
    after it is free-form per-station detail (line / shape / timing /
    target). Migrating a station onto the shared device removes its line.
    """
    entries = set()
    for line in LEGACY_MIGRATION_BACKLOG.read_text(encoding='utf-8').split('\n'):
        line = line.strip()
        if not line or line.startswith('#'):
            continue
        first = line.split()[0]
        if first.endswith('.py'):
            entries.add(first)
    return entries


def _code_names(tree):
    """Every identifier the code references (imports, names, attributes)."""
    names = set()
    for node in ast.walk(tree):
        if isinstance(node, ast.Name):
            names.add(node.id)
        elif isinstance(node, ast.Attribute):
            names.add(node.attr)
        elif isinstance(node, (ast.Import, ast.ImportFrom)):
            for alias in node.names:
                names.add(alias.asname or alias.name.split('.')[0])
    return names


_CHECK_MODE_FLAGS = ('--check', '--check-anchors')


def _names_check_mode(node):
    """A constant naming the check mode, or a sequence built only from them.

    ``['--check']``, ``("--check", "--check-anchors")`` and
    ``(['--check'], ['--check-anchors'])`` are the gate payloads the tree
    spells; a sequence mixing in any other literal does not name the check
    mode (widening that stays fail-closed — it would only add registrations,
    never lose a live gate).
    """
    if isinstance(node, ast.Constant) and node.value in _CHECK_MODE_FLAGS:
        return True
    if isinstance(node, (ast.List, ast.Tuple, ast.Set)):
        return bool(node.elts) and all(_names_check_mode(elt) for elt in node.elts)
    return False


def _has_check_mode_gate(tree):
    """A check-mode gate: a compare or attribute test naming the mode.

    Every spelling the scan covers, each pinned by a review round:
    ``<argv-ish> == ['--check']`` (single-element constant sequence — the
    fix-r6 predicate's only compare shape), the argparse
    ``if args.check:`` attribute gate, the membership tests
    ``'--check-anchors' in sys.argv`` and ``args in (["--check"],
    ["--check-anchors"])``, and the bare string compare
    ``mode == '--check-anchors'`` (#2795 fix-r7, boundary#1 + sibling#1:
    the fix-r6 predicate was blind to the last three and four live stations
    sailed past the scan). The gate matches at any ``if`` /
    conditional-expression position and through BoolOp / UnaryOp nesting:
    the compare walk covers the whole test expression, not just its top.
    The one live spelling outside this predicate — a gate through an
    intermediate boolean variable (``check_anchors = '--check-anchors' in
    sys.argv`` then ``if check_anchors:``) — is a registered blind spot
    (#2795 fix-r8, boundary#3: legacy-migration-backlog.txt「二（续三）」,
    pinned in test_intermediate_boolean_gate_spelling_is_a_registered_blind_spot).
    """
    for node in ast.walk(tree):
        if not isinstance(node, (ast.If, ast.IfExp)):
            continue
        for sub in ast.walk(node.test):
            if isinstance(sub, ast.Attribute) and sub.attr in ('check',
                                                               'check_anchors'):
                return True
            if not isinstance(sub, ast.Compare):
                continue
            for op, right in zip(sub.ops, sub.comparators):
                if isinstance(op, (ast.Eq, ast.In)) and (
                        _names_check_mode(sub.left) or _names_check_mode(right)):
                    return True
    return False


def bare_check_preflight_signature(text):
    """True when the source is a bare (unlocked, unsigned) check preflight.

    Gate shape present, but the file has no read-surface signature of its
    own (device call, alias imports included, or the legacy family message),
    does not dispatch through matrix_main, and uses no tree_lock anywhere
    -- the shape the review's P1 sampled by hand in 11 stations.
    """
    try:
        tree = ast.parse(text)
    except SyntaxError:
        # An unparseable gate file cannot prove any discipline: treat the
        # literal signatures as the only way out (fail-closed).
        return bool(_has_gate_literal(text) and not READ_SURFACE_MARKER.search(text)
                    and not ANCHOR_READ_WINDOW_CALL.search(text))
    if not _has_check_mode_gate(tree):
        return False
    if read_surface_signature(text):
        return False
    return not (_code_names(tree) & _COVERED_CODE_NAMES)


def _has_gate_literal(text):
    # The regex half of the gate detector for sources ast cannot parse —
    # every shape the AST half sees, all fail-closed: the literal compare
    # (constant sequence or bare string, either order), the membership test
    # (flag before or after the ``in``), and the argparse attribute spelling.
    # An unparseable gate file must not sail past (#2795 fix-r7 widened the
    # two literal-compare shapes to the membership spellings; fix-r8
    # boundary#2 pins this half with an unparseable synthetic fixture — until
    # then no test reached the SyntaxError branch at all).
    check = r'''['"]--check(?:-anchors)?['"]'''
    return bool(re.search(r'''==\s*\[\s*''' + check + r'''\s*\]''', text)
                or re.search(r'''==\s*''' + check, text)
                or re.search(check + r'''\s*==''', text)
                or re.search(check + r'''\s+in\s+''', text)
                or re.search(r'''\bin\b[^\n]*''' + check, text)
                or re.search(r'''if\s+\w+\.check(?:_anchors)?\b''', text))


def _bare_check_entries(root):
    """Scan a fixtures-shaped tree for unexempted bare check preflights."""
    flagged = set()
    for path in Path(root).rglob('*.py'):
        if path.name.startswith('test_') or '__pycache__' in path.parts:
            continue
        rel = str(path.relative_to(root))
        if rel in READ_SURFACE_SHARED_MODULES:
            continue
        if bare_check_preflight_signature(path.read_text(encoding='utf-8')):
            flagged.add(rel)
    return flagged


# The device definition and the shared matrix runner are not roster entries:
# negative_process.py defines the device (the refusal message lives only
# there) and negative_suite.py's matrix_main calls it on behalf of its
# callers; neither has a main() this roster could drive. Their coverage is
# every calling entry's roster row (channel-slack today) plus
# test_negative_suite.py, which drives matrix_main directly -- including its
# --check window's busy-lock refusal half (fix-r6:
# MatrixMainStartupTests.test_check_mode_refuses_when_the_tree_lock_is_busy;
# before that only channel-slack's own first window stood in front of the
# shared one).
READ_SURFACE_SHARED_MODULES = frozenset({
    'negative_process.py',
    'negative_suite.py',
})


class ReadSurfaceSpec:
    """One roster row: how to drive an entry's read-only anchor surface.

    ``argv`` drives the entry's own check/audit mode; ``root_name`` is the
    module attribute that names the entry's tree root (ROOT / root / REPO /
    repo -- the entries never agreed); ``dispatch_at_exec`` marks the entries
    whose dispatch runs at import time (no ``__main__`` guard: output-schema,
    task-budget-zero, js-math-zero, session-drop-release, session-factory,
    turn-runner/*) -- loading them with the test runner's own argv would start
    a mutation round, so their source executes with the check argv and the
    lock seam already patched; ``ok_codes`` lists the clean-run exit codes
    (workspace-pristine keeps two pre-existing #1666 stale anchors whose
    ``--check`` legitimately reports them as rc 1 -- recorded in RESULT).
    """

    def __init__(self, argv, root_name, dispatch_at_exec=False, ok_codes=(0,)):
        self.argv = tuple(argv)
        self.root_name = root_name
        self.dispatch_at_exec = dispatch_at_exec
        self.ok_codes = tuple(ok_codes)


READ_SURFACE_ROSTER = {
    # --check families (fix-r4 B 节，评审 sibling P1 同族跟改).
    'attach-followups-a/negative_controls.py': ReadSurfaceSpec(['--check'], 'ROOT'),
    'attach-twopass-1460/negative_controls.py': ReadSurfaceSpec(['--check'], 'ROOT'),
    'host-model-port/negative_controls.py': ReadSurfaceSpec(['--check'], 'ROOT'),
    'live-e2e-tmpdir/negative_controls.py': ReadSurfaceSpec(['--check'], 'ROOT'),
    'mode-routing/negative_controls.py': ReadSurfaceSpec(['--check'], 'ROOT'),
    'native-loop-stimulus/negative_controls.py': ReadSurfaceSpec(['--check'], 'ROOT'),
    'native-process/negative_controls.py': ReadSurfaceSpec(['--check'], 'ROOT'),
    'native-registry-switch/negative_controls.py': ReadSurfaceSpec(['--check'], 'ROOT'),
    'native-storage-stimulus/negative_controls.py': ReadSurfaceSpec(['--check'], 'ROOT'),
    'native-ws-entry/negative_controls.py': ReadSurfaceSpec(['--check'], 'ROOT'),
    'switch-state-machine/negative_controls.py': ReadSurfaceSpec(['--check'], 'ROOT'),
    'workspace-pristine/negative_controls.py': ReadSurfaceSpec(
        ['--check'], 'ROOT', ok_codes=(0, 1)),
    'issue-297/negative_controls.py': ReadSurfaceSpec(['--check'], 'root'),
    'issue-297-domain-fold/negative_controls.py': ReadSurfaceSpec(['--check'], 'root'),
    'sdk-cli-lifecycle/negative_controls.py': ReadSurfaceSpec(['--check'], 'root'),
    'sdk-format/negative_controls.py': ReadSurfaceSpec(['--check'], 'root'),
    'sdk-run/negative_controls.py': ReadSurfaceSpec(['--check'], 'root'),
    'sdk-sessions-list/negative_controls.py': ReadSurfaceSpec(['--check'], 'root'),
    'sdk-sessions-messages/negative_controls.py': ReadSurfaceSpec(['--check'], 'root'),
    'sdk-settings/negative_controls.py': ReadSurfaceSpec(['--check'], 'root'),
    'sdk-terminal/negative_controls.py': ReadSurfaceSpec(['--check'], 'root'),
    'sdk-mcp-session-tools/negative_controls.py': ReadSurfaceSpec(['--check'], 'ROOT'),
    'js-math-zero/negative.py': ReadSurfaceSpec(['--check'], 'root', dispatch_at_exec=True),
    'task-budget-zero/negative.py': ReadSurfaceSpec(['--check'], 'root', dispatch_at_exec=True),
    # --check-anchors families (fix-r3 的 21 个入口 + fix-r4 同形).
    'closure_guard_negative.py': ReadSurfaceSpec(['--check-anchors'], 'REPO'),
    'p01-preflight/negative_controls.py': ReadSurfaceSpec(['--check-anchors'], 'REPO'),
    'gateway-entry-guard/negative_controls.py': ReadSurfaceSpec(['--check-anchors'], 'ROOT'),
    'parity-lock-release/negative_controls.py': ReadSurfaceSpec(['--check-anchors'], 'ROOT'),
    'session-domain-expectations/negative_controls.py': ReadSurfaceSpec(
        ['--check-anchors'], 'ROOT'),
    'session-domain-lock-release/negative_controls.py': ReadSurfaceSpec(
        ['--check-anchors'], 'ROOT'),
    'session-domain-native/negative_controls.py': ReadSurfaceSpec(
        ['--check-anchors'], 'ROOT'),
    'handle-path-macos/negative_controls.py': ReadSurfaceSpec(['--check'], 'ROOT'),
    'mcp-gateway/negative_controls.py': ReadSurfaceSpec(['--check-anchors'], 'root'),
    'native-collation/negative_controls.py': ReadSurfaceSpec(['--check-anchors'], 'root'),
    'native-session-e2e/negative_controls.py': ReadSurfaceSpec(['--check-anchors'], 'root'),
    'native-stimulus-plan/negative_controls.py': ReadSurfaceSpec(['--check-anchors'], 'root'),
    'native-witness/negative.py': ReadSurfaceSpec(['--check-anchors'], 'root'),
    'same-json-surfaces/negative_controls.py': ReadSurfaceSpec(['--check-anchors'], 'root'),
    'skills-external/negative_controls.py': ReadSurfaceSpec(['--check-anchors'], 'root'),
    'utf16-residual-sort/negative_controls.py': ReadSurfaceSpec(['--check-anchors'], 'root'),
    'model-http-encoding/negative.py': ReadSurfaceSpec(['--check-anchors'], 'root'),
    'output-schema/negative.py': ReadSurfaceSpec(
        ['--check-anchors'], 'repo', dispatch_at_exec=True),
    # --audit families (fix-r2/fix-r3 先例 + fix-r4 同形).
    'manager-gateway/negative_controls.py': ReadSurfaceSpec(['--audit'], 'root'),
    'snapshot-projection/negative_controls.py': ReadSurfaceSpec(['--audit'], 'ROOT'),
    'mcp-http-corrupt-body/negative_controls.py': ReadSurfaceSpec(['--audit'], 'root'),
    'mcp-http-encoding/negative.py': ReadSurfaceSpec(['--audit'], 'root'),
    'permission-mode-surrogate/negative_controls.py': ReadSurfaceSpec(['--audit'], 'root'),
    'transcript-trim/negative_controls.py': ReadSurfaceSpec(['--audit'], 'root'),
    'session-factory/negative_controls.py': ReadSurfaceSpec(
        ['--audit'], 'root', dispatch_at_exec=True),
    'session-factory/append-delta/negative_controls.py': ReadSurfaceSpec(
        ['--audit'], 'root'),
    'session-factory/projection-cache/negative_controls.py': ReadSurfaceSpec(
        ['--audit'], 'root'),
    'session-drop-release/negative_controls.py': ReadSurfaceSpec(
        ['--audit'], 'root', dispatch_at_exec=True),
    'turn-runner/forward_negative.py': ReadSurfaceSpec(
        ['--audit'], 'root', dispatch_at_exec=True),
    'turn-runner/integrity_negative.py': ReadSurfaceSpec(
        ['--audit'], 'root', dispatch_at_exec=True),
    # issue-2495: #2590 fix-r3b 的同族先例，拒跑消息是自己的 stale 措辞；
    # 名册登记后同样被「锁内读」机检覆盖（其专用 mismatch 机检在
    # RegisteredEntryRosterGuard，二者互补不重复）。
    'issue-2495/negative_controls.py': ReadSurfaceSpec(['--check'], 'ROOT'),
    # #2795 fix-r5（评审 sibling P1#1/P1#2/P1#3 + P2#4 同类跟改）：
    # 三个此前完全裸读的入口改走共享 anchor_read_window 装置并登记——
    # frozen-reinsert-guard 与 background-panic 的 --check-anchors、
    # channel-slack 的 --check（preflight + matrix_main 共享路径）；
    # upload-gateway 的 --check 同为裸读（评审 signature 盲区外的同形漏网，
    # 补 dispatch-registration 变异行时一并收口），且在 import 期分发。
    'frozen-reinsert-guard/negative_controls.py': ReadSurfaceSpec(
        ['--check-anchors'], 'ROOT'),
    'background-panic/negative_controls.py': ReadSurfaceSpec(
        ['--check-anchors'], 'root'),
    'channel-slack/negative_controls.py': ReadSurfaceSpec(['--check'], 'ROOT'),
    'upload-gateway/negative_controls.py': ReadSurfaceSpec(
        ['--check'], 'root', dispatch_at_exec=True),
}


class ReadSurfaceDrive:
    """One driven read-only run of a roster entry's real check/audit mode."""

    def __init__(self, code, stdout, stderr, events, lock_calls, root):
        self.code = code
        self.stdout = stdout
        self.stderr = stderr
        self.events = events
        self.lock_calls = lock_calls
        self.root = root


# The in-drive half of the read-surface machine check. It runs as its own
# ``python3 -I`` process because four roster entries (native-session-e2e,
# native-stimulus-plan, native-witness, same-json-surfaces) refuse any
# non-isolated interpreter at module top -- the roster drives every entry
# the same way, through a real isolated interpreter with a real argv, which
# is also closer to how CI runs the entries than an in-process main() call.
# Only the read surface's seams are stubbed: ``negative_process.tree_lock``
# becomes a recording fake (refusing on a busy lock) and ``Path.read_text``
# / ``Path.read_bytes`` record every read that happens with the lock not
# held. The argv parsing, the roster checks, the real anchors and the
# refusal shapes all run through the entry's real code; no cargo, no tree
# mutation, no evidence directory. The driver prints exactly one JSON line.
_READ_SURFACE_DRIVER = '''
import importlib.util
import io
import json
import os
import pathlib
import sys
from contextlib import redirect_stdout, redirect_stderr

payload = json.loads(pathlib.Path(sys.argv[1]).read_text())
fixtures = pathlib.Path(payload["fixtures"])
entry_dir = (fixtures / payload["script"]).parent
# A real script launch puts the entry's own directory on sys.path (for
# turn-runner's _negative_common); ``-I`` strips it, so put it back.
sys.path.insert(0, str(fixtures))
sys.path.insert(0, str(entry_dir))
import negative_process

events = []

class RecordingTreeLock:
    def __init__(self, busy):
        self.busy = busy
        self.held = False
        self.calls = []
    def __call__(self, *args, **kwargs):
        self.calls.append([[str(arg) for arg in args], kwargs])
        return self
    def __enter__(self):
        if self.busy:
            raise TimeoutError("read-surface drive: lock timeout (#2795 fix-r4)")
        events.append("lock-enter")
        self.held = True
        return self
    def __exit__(self, *_exc):
        self.held = False
        events.append("lock-exit")
        return False

lock = RecordingTreeLock(payload["busy"])
real_read_text = pathlib.Path.read_text
real_read_bytes = pathlib.Path.read_bytes

def pinned_read_text(self, *args, **kwargs):
    if not lock.held:
        events.append(["unlocked-read", "read_text", str(self)])
    return real_read_text(self, *args, **kwargs)

def pinned_read_bytes(self, *args, **kwargs):
    # output-schema audits byte anchors; the same held-lock pin must sit on
    # the bytes read, not only on the text one.
    if not lock.held:
        events.append(["unlocked-read", "read_bytes", str(self)])
    return real_read_bytes(self, *args, **kwargs)

module = None
code = "driver: the entry did not exit"
stdout, stderr = io.StringIO(), io.StringIO()
os.environ.update(payload["env"])
sys.argv = [payload["wrapper"], *payload["argv"]]
pathlib.Path.read_text = pinned_read_text
pathlib.Path.read_bytes = pinned_read_bytes
try:
    with redirect_stdout(stdout), redirect_stderr(stderr):
        entry = fixtures / payload["script"]
        spec = importlib.util.spec_from_file_location("read_surface_entry", entry)
        module = importlib.util.module_from_spec(spec)
        saved = sys.path[:]
        try:
            negative_process.tree_lock = lock
            spec.loader.exec_module(module)
        finally:
            sys.path[:] = saved
        if not payload["dispatch_at_exec"]:
            module.tree_lock = lock
            code = module.main()
        else:
            code = "dispatch ran to import completion without exiting"
except SystemExit as error:
    # ``raise SystemExit()`` (no code) is a process exit status of 0; a
    # string code is a usage error the assertions reject by shape.
    if error.code is None:
        code = 0
    elif isinstance(error.code, int):
        code = error.code
    else:
        code = "sys.exit(%r)" % (error.code,)
except (TimeoutError, OSError, UnicodeDecodeError) as error:
    # A refusal must be a returned 2 with the family message, never an
    # exception escaping the entry (a bare traceback reads as rc 1 on the
    # host and carries no dedicated refusal message).
    code = "uncaught %s: %s" % (type(error).__name__, error)
except ValueError as error:
    code = "uncaught ValueError: %s" % (error,)
finally:
    pathlib.Path.read_text = real_read_text
    pathlib.Path.read_bytes = real_read_bytes
root = getattr(module, payload["root_name"], None)
print(json.dumps({
    "code": code,
    "stdout": stdout.getvalue(),
    "stderr": stderr.getvalue(),
    "events": events,
    "lock_calls": lock.calls,
    "root": None if root is None else str(root),
}))
'''


def _drive_read_surface(testcase, script, spec, *, busy=False):
    """Drive one roster entry's read-only anchor surface through real code."""
    base = Path(tempfile.mkdtemp(prefix='negctl-2795r4-'))
    testcase.addCleanup(shutil.rmtree, base, ignore_errors=True)
    driver = base / 'read-surface-driver.py'
    driver.write_text(_READ_SURFACE_DRIVER)
    payload = base / 'payload.json'
    payload.write_text(json.dumps({
        'fixtures': str(FIXTURES),
        'script': script,
        'argv': list(spec.argv),
        'busy': busy,
        'dispatch_at_exec': spec.dispatch_at_exec,
        'root_name': spec.root_name,
        'wrapper': str(base / 'wrapper-launch.py'),
        'env': _environment(base),
    }))
    try:
        result = subprocess.run(
            [sys.executable, '-I', '-B', str(driver), str(payload)],
            cwd=str(FIXTURES), capture_output=True, text=True, timeout=300)
    except subprocess.TimeoutExpired:
        return ReadSurfaceDrive('driver timeout after 300 s', '', '', [], [], None)
    if result.returncode != 0:
        return ReadSurfaceDrive(
            f'driver exit {result.returncode}: {result.stderr[-400:]!r}',
            result.stdout, result.stderr, [], [], None)
    report = json.loads(result.stdout)
    return ReadSurfaceDrive(report['code'], report['stdout'], report['stderr'],
                            report['events'], report['lock_calls'],
                            report['root'])


class ReadOnlyAnchorReadSurfaceRoster(unittest.TestCase):
    """The read-only anchor entry roster: every read happens inside the lock.

    Each test loops the whole register so a row cannot be added without
    inheriting both pins (reads-inside-the-lock and the busy-lock refusal),
    and the scan keeps the register honest in both directions: an entry that
    grows the family refusal without registering fails the scan, and a
    registered entry that stops matching the signature fails it the same way
    (the defence left the source, so the register row is stale).
    """

    def test_registered_entries_exist(self):
        for script, spec in READ_SURFACE_ROSTER.items():
            with self.subTest(script=script):
                self.assertTrue((FIXTURES / script).is_file(),
                                f'#2795 {script} is registered but does not exist')
                # A vacuous row disarms the loop assertions silently: the
                # argv, the root name and the ok codes must all be real.
                self.assertTrue(spec.argv and spec.root_name and spec.ok_codes,
                                f'#2795 {script}: the roster row must carry a '
                                'mode argv, a root constant name and clean-run '
                                'exit codes')

    def test_every_read_only_anchor_entry_is_registered(self):
        found = {str(path.relative_to(FIXTURES))
                 for path in FIXTURES.rglob('*.py')
                 if not path.name.startswith('test_')
                 and str(path.relative_to(FIXTURES))
                 not in READ_SURFACE_SHARED_MODULES
                 and read_surface_signature(path.read_text(encoding='utf-8'))}
        self.assertTrue(found == set(READ_SURFACE_ROSTER),
                        f'#2795 every entry whose read-only anchor surface '
                        f'refuses through the family message or calls the '
                        f'shared anchor_read_window device must be '
                        f'machine-checked through this roster '
                        f'(unregistered={sorted(found - set(READ_SURFACE_ROSTER))} '
                        f'stale={sorted(set(READ_SURFACE_ROSTER) - found)})')

    def test_the_scan_detects_a_new_family_entry(self):
        # The kill shape the scan exists for: the next batch adds the same
        # family defence to another entry and forgets the register row. Both
        # signatures are demonstrated on a synthetic tree (never the real one):
        # the legacy family message, and a device call whose label/message a
        # fourth spelling could never drift (#2795 fix-r5 结构性收口——装置名
        # 即登记签名，文案只写在装置里).
        # #2795 fix-r6（boundary#2）：import 别名形态同测——
        # ``from negative_process import anchor_read_window as w`` 的调用点
        # 也必须被名册签名认出，否则别名入口的防务扫不到（假 PASS）。
        with tempfile.TemporaryDirectory(prefix='negctl-2795r4-scan-') as base:
            legacy = Path(base) / 'new-family' / 'negative_controls.py'
            legacy.parent.mkdir()
            legacy.write_text("print('tree lock or anchor read failed: demo')\n")
            device_based = Path(base) / 'new-device-family' / 'negative_controls.py'
            device_based.parent.mkdir()
            device_based.write_text(
                'from negative_process import anchor_read_window\n'
                "with anchor_read_window(ROOT, 'demo'):\n    pass\n")
            aliased = Path(base) / 'new-alias-family' / 'negative_controls.py'
            aliased.parent.mkdir()
            aliased.write_text(
                'from negative_process import anchor_read_window as w\n'
                "with w(ROOT, 'demo'):\n    pass\n")
            def scan(tree_root):
                return {str(path.relative_to(tree_root))
                        for path in Path(tree_root).rglob('*.py')
                        if not path.name.startswith('test_')
                        and str(path.relative_to(tree_root))
                        not in READ_SURFACE_SHARED_MODULES
                        and read_surface_signature(path.read_text(encoding='utf-8'))}
            found = scan(base)
        self.assertTrue(found == {'new-family/negative_controls.py',
                                 'new-device-family/negative_controls.py',
                                 'new-alias-family/negative_controls.py'}
                        and 'new-family/negative_controls.py'
                        not in READ_SURFACE_ROSTER,
                        '#2795 the scan predicate must flag a new same-family '
                        'entry through either signature (legacy message, '
                        'shared device call or an aliased import of it), '
                        'which the register test above then reports as '
                        'unregistered (red)')

    def test_each_entry_reads_only_inside_the_tree_lock(self):
        repo_root = str(FIXTURES.parents[3])
        for script, spec in sorted(READ_SURFACE_ROSTER.items()):
            with self.subTest(script=script):
                run = _drive_read_surface(self, script, spec)
                self.assertIn(run.code, spec.ok_codes,
                              f'#2795 {script}: the clean read-only run must '
                              f'exit in {spec.ok_codes}, got {run.code!r}; '
                              f'stderr={run.stderr[:400]!r}')
                unlocked = [event for event in run.events
                           if isinstance(event, list)]
                self.assertTrue(not unlocked,
                                f'#2795 {script}: every anchor read must happen '
                                f'with the shared tree lock held (a sibling '
                                f'runner may have a mutant on disk between its '
                                f'write and restore phases): {unlocked[:3]!r}')
                self.assertTrue(run.events
                                and run.events[0] == 'lock-enter'
                                and run.events[-1] == 'lock-exit',
                                f'#2795 {script}: the lock must wrap the reads, '
                                f'not merely be taken: {run.events!r}')
                # The lock argument is the pin the drive exists for: the
                # entry's own module-level root const (ROOT / root / REPO /
                # repo -- they never agreed), which is always the repo root,
                # always with the 60 s bounded wait.
                self.assertTrue(run.root == repo_root,
                                f'#2795 {script}: {spec.root_name} must name '
                                f'the repo root {repo_root}, got {run.root!r}')
                self.assertTrue(run.lock_calls
                                and all(call == [[run.root], {'timeout': 60}]
                                        for call in run.lock_calls),
                                f'#2795 {script}: the read surface must lock '
                                f'the entry\'s own tree root with the 60 s '
                                f'bounded wait, got {run.lock_calls!r}')

    def test_each_entry_refuses_when_the_tree_lock_is_busy(self):
        for script, spec in sorted(READ_SURFACE_ROSTER.items()):
            with self.subTest(script=script):
                source = (FIXTURES / script).read_text(encoding='utf-8')
                run = _drive_read_surface(self, script, spec, busy=True)
                self.assertTrue(run.code == 2,
                                f'#2795 {script}: a busy tree lock is a '
                                f'refusal to start (2), got {run.code!r}')
                if ANCHOR_READ_WINDOW_CALL.search(source):
                    # fix-r5 结构性收口：装置化入口的忙锁拒跑必须走装置的
                    # 统一文案 + 异常类型名（文案只写在装置里，入口不再有自
                    # 己的拼写），且认得到入口源里的字面标签之一。
                    self.assertIn('tree lock or anchor read failed', run.stderr,
                                  f'#2795 {script}: the device-based refusal '
                                  f'must use the unified message: '
                                  f'{run.stderr[:400]!r}')
                    self.assertIn('TimeoutError', run.stderr,
                                  f'#2795 {script}: the device names the '
                                  f'refusal cause: {run.stderr[:400]!r}')
                    labels = ANCHOR_READ_WINDOW_LABEL.findall(source)
                    self.assertTrue(
                        not labels or any(label in run.stderr for label in labels),
                        f'#2795 {script}: the refusal must carry one of the '
                        f'entry\'s device labels {labels!r}: '
                        f'{run.stderr[:400]!r}')
                else:
                    self.assertIn('tree lock or', run.stderr,
                                  f'#2795 {script}: the refusal must name the '
                                  f'lock wait on stderr: {run.stderr[:400]!r}')
                self.assertTrue(run.events == [],
                                f'#2795 {script}: a refused run must read no '
                                f'anchors: {run.events!r}')


class BareCheckPreflightBacklog(unittest.TestCase):
    """#2795 fix-r6（评审 sibling P1）：裸读检查预检的续跑表豁免机检。

    名册签名（装置调用/旧文案）扫不到既无锁也无文案的 --check /
    --check-anchors 预检——评审按字面形状取样 11 站判 P1。本类把「同族
    全量清扫」机检化：扫描按 AST 形状命中全部裸读站，续跑表
    （legacy-migration-backlog.txt，单一真相源）是唯一豁免清单；长出新
    裸读站不登记就红，登记的行不存在于树（迁移完忘删）也红。
    """

    def test_every_bare_check_preflight_is_in_the_backlog(self):
        unregistered = _bare_check_entries(FIXTURES) - backlog_exempted_entries()
        self.assertTrue(not unregistered,
                        '#2795 fix-r6: every check-mode (--check / '
                        '--check-anchors) entry whose preflight has no device '
                        'call (alias imports included), no legacy family '
                        'message, no matrix_main dispatch and no tree_lock of '
                        'its own is a bare sibling-read window -- a sibling '
                        'mutation runner can leave a mutant on disk between '
                        'its write and restore phases, and the unlocked '
                        'preflight validates anchors against bytes it does not '
                        'own. Register it in legacy-migration-backlog.txt or '
                        'migrate it onto anchor_read_window '
                        f'(unregistered={sorted(unregistered)})')

    def test_backlog_entries_exist_in_the_tree(self):
        # The exemption must not silently disarm itself: a stale row (its
        # entry renamed away or fully migrated and deleted from the tree)
        # keeps the scan green for a path nothing can hit anymore.
        for entry in sorted(backlog_exempted_entries()):
            with self.subTest(entry=entry):
                self.assertTrue((FIXTURES / entry).is_file(),
                                f'#2795 fix-r6: {entry} is exempted in '
                                'legacy-migration-backlog.txt but does not '
                                'exist in the tree; a stale exemption row '
                                'must be removed together with the entry')

    def test_the_bare_scan_detects_each_kill_shape(self):
        # The scan predicate's every half, demonstrated on a synthetic tree
        # (never the real one): a bare gate+read entry is flagged — in every
        # gate spelling the scan covers (literal compare, the argparse
        # ``if args.check:`` form, the ``'--check-anchors' in sys.argv``
        # membership test, the bare ``mode == '--check-anchors'`` string
        # compare and the ``args in (["--check"], ["--check-anchors"])``
        # alias-tuple membership — #2795 fix-r7, boundary#1 + sibling#1: the
        # fix-r6 predicate only saw the first two shapes and four live
        # stations sailed past; the one spelling outside the predicate — the
        # intermediate-boolean gate — is a registered blind spot, fix-r8
        # boundary#3, pinned in its own test below); an unparseable source
        # with the gate literal is flagged through _has_gate_literal, the
        # regex half for sources ast cannot parse (fix-r8, boundary#2: no
        # test reached the SyntaxError branch before); and a bare entry whose
        # code merely uses plausible-sounding identifiers (out /
        # registrations / selected) stays flagged (fix-r8, boundary#1:
        # those names joining _COVERED_CODE_NAMES would silently exempt real
        # tree files). The same entry behind an aliased device import,
        # behind the legacy message, behind a matrix_main dispatch, and a
        # file whose gate is bare but which holds a tree_lock elsewhere
        # (the 「二（续）」manual-registration shape — fix-r7, sibling#2) are
        # not.
        with tempfile.TemporaryDirectory(prefix='negctl-2795r6-scan-') as base:
            base = Path(base)
            bare = base / 'new-bare' / 'negative_controls.py'
            bare.parent.mkdir()
            bare.write_text("import sys\n"
                            "if sys.argv[1:] == ['--check']:\n"
                            "    for row in rows:\n"
                            "        check_anchor(row, row[1].read_text())\n")
            argparse_bare = base / 'new-argparse-bare' / 'negative_controls.py'
            argparse_bare.parent.mkdir()
            argparse_bare.write_text("import argparse\n"
                                     "parser = argparse.ArgumentParser()\n"
                                     "parser.add_argument('--check', action='store_true')\n"
                                     "args = parser.parse_args()\n"
                                     "if args.check:\n"
                                     "    for row in rows:\n"
                                     "        check_anchor(row, row[1].read_text())\n")
            aliased = base / 'new-aliased' / 'negative_controls.py'
            aliased.parent.mkdir()
            aliased.write_text(
                'from negative_process import anchor_read_window as w\n'
                "if args == ['--check']:\n"
                "    with w(ROOT, 'demo'):\n"
                "        for row in rows:\n"
                "            check_anchor(row, row[1].read_text())\n")
            legacy = base / 'new-legacy' / 'negative_controls.py'
            legacy.parent.mkdir()
            legacy.write_text("if args == ['--check']:\n"
                             "    for row in rows:\n"
                             "        check_anchor(row, row[1].read_text())\n"
                             "print('tree lock or anchor read failed: demo')\n")
            in_argv_bare = base / 'new-in-argv-bare' / 'negative_controls.py'
            in_argv_bare.parent.mkdir()
            in_argv_bare.write_text("import sys\n"
                                    "if '--check-anchors' in sys.argv:\n"
                                    "    sources = {path: path.read_text()\n"
                                    "               for path in paths}\n")
            mode_compare_bare = base / 'new-mode-compare-bare' / 'negative_controls.py'
            mode_compare_bare.parent.mkdir()
            mode_compare_bare.write_text("if mode == '--check-anchors':\n"
                                         "    for row in rows:\n"
                                         "        check_anchor(row, row[1].read_text())\n")
            tuple_membership_bare = \
                base / 'new-tuple-membership-bare' / 'negative_controls.py'
            tuple_membership_bare.parent.mkdir()
            tuple_membership_bare.write_text(
                "args = sys.argv[1:]\n"
                "if args in (['--check'], ['--check-anchors']):\n"
                "    for row in rows:\n"
                "        check_anchor(row, row[1].read_text())\n")
            boolop_bare = base / 'new-boolop-bare' / 'negative_controls.py'
            boolop_bare.parent.mkdir()
            boolop_bare.write_text("import sys\n"
                                   "if args == ['--check'] or "
                                   "'--check-anchors' in sys.argv:\n"
                                   "    for row in rows:\n"
                                   "        check_anchor(row, row[1].read_text())\n")
            tree_locked = base / 'new-tree-locked' / 'negative_controls.py'
            tree_locked.parent.mkdir()
            tree_locked.write_text("from negative_process import tree_lock\n"
                                   "if '--check-anchors' in sys.argv:\n"
                                   "    for row in rows:\n"
                                   "        check_anchor(row, row[1].read_text())\n"
                                   "with tree_lock(ROOT, timeout=900):\n"
                                   "    pass\n")
            # #2795 fix-r8（boundary#1）：parseable matrix_main dispatch —
            # 共享 runner 持装置窗口，dispatch 入口本身不该被点名；旧夹具
            # （fix-r6 带模块级 return）被 fix-r7 删掉后，此豁免名不再有
            # 夹具钉。
            dispatched = base / 'new-dispatched' / 'negative_controls.py'
            dispatched.parent.mkdir()
            dispatched.write_text("from negative_suite import matrix_main\n"
                                 "if args == ['--check']:\n"
                                 "    matrix_main(ROOT, __file__, 'demo', rows)\n")
            # #2795 fix-r8（boundary#1）：常用标识符（out / registrations /
            # selected）在裸站代码里出现不构成任何纪律——这些名字若进
            # _COVERED_CODE_NAMES，真实树大批文件会被静默豁免。
            common_named = base / 'new-common-named-bare' / 'negative_controls.py'
            common_named.parent.mkdir()
            common_named.write_text("if args == ['--check']:\n"
                                    "    out = {}\n"
                                    "    registrations = []\n"
                                    "    for row in rows:\n"
                                    "        selected = row[1].read_text()\n"
                                    "        out[row[0]] = selected\n"
                                    "        registrations.append(row[0])\n")
            # #2795 fix-r8（boundary#2）：语法残缺但含门字面——ast 半边不可
            # 达，必须由 _has_gate_literal 正则半边标出。
            unparseable = base / 'new-unparseable-bare' / 'negative_controls.py'
            unparseable.parent.mkdir()
            unparseable.write_text("import sys\n"
                                   "if '--check-anchors' in sys.argv:\n"
                                   "    sources = {path: path.read_text()\n")
            found = _bare_check_entries(base)
        self.assertTrue(found == {'new-bare/negative_controls.py',
                                  'new-argparse-bare/negative_controls.py',
                                  'new-in-argv-bare/negative_controls.py',
                                  'new-mode-compare-bare/negative_controls.py',
                                  'new-tuple-membership-bare/'
                                  'negative_controls.py',
                                  'new-boolop-bare/negative_controls.py',
                                  'new-common-named-bare/negative_controls.py',
                                  'new-unparseable-bare/'
                                  'negative_controls.py'},
                        '#2795 fix-r6: the bare-check scan must flag the '
                        'gate+read entry with no discipline of its own in '
                        'every gate spelling the scan covers (literal '
                        'compare, argparse attribute, argv membership, '
                        'bare string compare, '
                        'alias-tuple membership, BoolOp nesting), plus the '
                        'unparseable source with the gate literal (regex '
                        'half, fix-r8 boundary#2) and the entry whose code '
                        'merely uses plausible identifiers — out / '
                        'registrations / selected exempt nothing (fix-r8 '
                        'boundary#1) — and only those: an entry behind a '
                        'device import (aliased or not), the legacy message, '
                        'a matrix_main dispatch, or a tree_lock held '
                        'elsewhere in the file is the registered '
                        f'manual-exemption shape: {sorted(found)}')

    def test_intermediate_boolean_gate_spelling_is_a_registered_blind_spot(self):
        """#2795 fix-r8（boundary#3，登记不修）：中间布尔变量门是登记的谓词盲区。

        门经 ``check_anchors = '--check-anchors' in sys.argv`` 后接
        ``if check_anchors:`` 拼写时，_has_check_mode_gate 扫不到：该
        Compare 在 ``ast.Assign`` 里、``if`` 的 test 是裸 Name，谓词三分支
        都不命中。树内活体 output-schema/negative.py 靠 tree_lock import
        与续跑表一节行多重豁免，非现存假 PASS；同形新裸站扫描不会点名，
        只能人工核对后登记（续跑表「二（续三）」节）。本钉两面：合成
        同形裸站不被点名（盲区如实——把谓词扩到 Assign 形状必须连全树
        重扫、登记迁移与本钉一起交，不许静默改覆盖面）；同树里可检测
        形的新裸站仍被点名（盲区不得解除扫描对可见形状的武装）；登记
        节本身必须留在续跑表（删节没有任何功能后果，只有本钉红）。
        """
        with tempfile.TemporaryDirectory(prefix='negctl-2795r8-blind-') as base:
            base = Path(base)
            intermediate = \
                base / 'new-intermediate-bool-bare' / 'negative_controls.py'
            intermediate.parent.mkdir()
            intermediate.write_text("import sys\n"
                                    "check_anchors = '--check-anchors' in sys.argv\n"
                                    "if check_anchors:\n"
                                    "    sources = {path: path.read_text()\n"
                                    "               for path in paths}\n")
            detectable = base / 'new-detectable-bare' / 'negative_controls.py'
            detectable.parent.mkdir()
            detectable.write_text("if args == ['--check']:\n"
                                 "    for row in rows:\n"
                                 "        check_anchor(row, row[1].read_text())\n")
            found = _bare_check_entries(base)
        self.assertTrue(found == {'new-detectable-bare/negative_controls.py'},
                        '#2795 fix-r8: the intermediate-boolean gate spelling '
                        '(check_anchors = ... in sys.argv; if check_anchors:) '
                        'is a registered scan blind spot, so the synthetic '
                        'same-shape bare station must sail past while the '
                        'detectable new bare station in the same tree is '
                        'still flagged — extending _has_check_mode_gate to '
                        'the Assign shape must rescan the tree, move the '
                        'backlog registration and update this pin together, '
                        f'never silently change coverage: {sorted(found)}')
        backlog = LEGACY_MIGRATION_BACKLOG.read_text(encoding='utf-8')
        self.assertTrue('中间布尔变量' in backlog,
                        '#2795 fix-r8: the intermediate-boolean gate '
                        'blind-spot registration must stay in '
                        'legacy-migration-backlog.txt（「二（续三）」节）： '
                        'deleting the note has no functional consequence, '
                        'only this pin goes red')

    def test_known_blind_spot_registrations_stay_in_the_backlog(self):
        """#2795 fix-r8（concurrency#1，登记处置）：登记的盲区行是交付物本体。

        test_* 前缀文件不进裸读扫描，--negative 元控的变体循环读面
        （DriftTests.setUp 把 gate.tree_lock 换记录桩，16 个变体的
        gate.main()/check_anchors() 读 PANEL/SESSION 无锁）因此扫描根本
        看不见——登记行删掉没有任何功能后果，只有本钉红。迁移
        （变体循环包进 anchor_read_window）完成时随登记行删本断言。
        """
        held = backlog_exempted_entries()
        self.assertTrue(
            'session-domain-expectations/test_meta_controls.py' in held,
            '#2795 fix-r8: the --negative meta-control variant-loop read '
            'face (16 variants reading PANEL/SESSION under the setUp '
            'recording stub) is registered-only — the bare-check scan '
            'excludes test_* files, so deleting the backlog row has no '
            'functional consequence and only this pin goes red; if the '
            'variant loop has been migrated onto anchor_read_window, drop '
            'the row and this assertion together')


class BackgroundPanicRunRefusal(unittest.TestCase):
    """#2795 fix-r6（评审 sibling#6/concurrency#1）：拒跑不落证据目录。

    background-panic 的 run 模式此前先 ``out.mkdir`` 再进 fix-r5 新加的
    锚点窗口——锁忙拒跑（rc 2）发生在目录创建之后，会留下一个空证据目录，
    重跑同路径必 ``FileExistsError``，破坏「拒跑=从未开始、不占证据目录」
    契约（matrix_main 的 test_negative_suite.py 同形明测）。本类驱动真实
    main() 的 run 模式：忙锁 → SystemExit(2) + 装置统一文案，且目录未建。
    """

    def test_a_busy_lock_refusal_leaves_no_evidence_directory(self):
        module = load_entry('background-panic/negative_controls.py')
        base = Path(tempfile.mkdtemp(prefix='negctl-2795r6-bp-'))
        self.addCleanup(shutil.rmtree, base, ignore_errors=True)
        out = base / 'out'
        errors = io.StringIO()
        with patch.dict(os.environ, {'CARGO_TARGET_DIR': str(base / 'cargo-target')}), \
                patch.object(negative_process, 'tree_lock',
                             _RecordingTreeLock([], busy=True)), \
                patch('sys.argv', [str(base / 'wrapper-launch.py'), str(out)]), \
                contextlib.redirect_stderr(errors):
            with self.assertRaises(SystemExit) as caught:
                module.main()
        self.assertTrue(caught.exception.code == 2,
                        '#2795 fix-r6: a busy tree lock must refuse the run '
                        f'with rc 2, got {caught.exception.code!r}')
        self.assertTrue(
            'background-panic anchor preflight: '
            'tree lock or anchor read failed' in errors.getvalue()
            and 'TimeoutError' in errors.getvalue(),
            f'#2795 fix-r6: the refusal must use the shared device message '
            f'and name the lock wait: {errors.getvalue()!r}')
        self.assertTrue(not out.exists(),
                        '#2795 fix-r6: a refused run must leave no evidence '
                        'directory (mkdir happens only after the preflight '
                        'window; a re-run of the same path must not hit '
                        'FileExistsError)')


class ChannelSlackRunPreflightWindow(unittest.TestCase):
    """#2795 fix-r6（评审 sibling#2）：run 分支 preflight 的装置窗口。

    channel-slack 的 --check 分支 fix-r5 已收口，run 分支的
    ``mutant_switch.preflight`` 此前保持裸读——mutant_switch.run 的 pristine
    校验在其内部（取锁之后），preflight 先读仍可能被兄弟变异窗口打成假
    拒跑（stale ValueError → 外层 catch → rc 2 ``preflight:``，合法实跑被
    误归因）。本类驱动真实 main() 的 run 分支：忙锁 → SystemExit(2) +
    装置统一文案（不进外层 ValueError/OSError catch）。
    """

    def test_run_mode_preflight_happens_inside_the_device_window(self):
        module = load_entry('channel-slack/negative_controls.py')
        base = Path(tempfile.mkdtemp(prefix='negctl-2795r6-cs-'))
        self.addCleanup(shutil.rmtree, base, ignore_errors=True)
        events = []
        errors = io.StringIO()
        with patch.dict(os.environ, {'CARGO_TARGET_DIR': str(base / 'cargo-target')}), \
                patch.object(negative_process, 'tree_lock',
                             _RecordingTreeLock(events, busy=True)), \
                patch('sys.argv', [str(base / 'wrapper-launch.py')]), \
                contextlib.redirect_stderr(errors):
            with self.assertRaises(SystemExit) as caught:
                module.main()
        self.assertTrue(caught.exception.code == 2,
                        '#2795 fix-r6: a busy tree lock must refuse the run '
                        f'before any preflight read, got {caught.exception.code!r}')
        self.assertTrue(
            'channel-slack preflight: tree lock or anchor read failed'
            in errors.getvalue()
            and 'TimeoutError' in errors.getvalue(),
            f'#2795 fix-r6: the run-mode refusal must use the shared device '
            f'message and name the lock wait (a bare preflight that falls '
            f'through to the outer ValueError catch prints '
            f'"preflight: ..." instead): {errors.getvalue()!r}')
        self.assertTrue(events == [],
                        f'#2795 fix-r6: a refused run-mode preflight must read '
                        f'no switch sources: {events!r}')


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
