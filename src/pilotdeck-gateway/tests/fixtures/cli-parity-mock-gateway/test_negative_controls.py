# anchor-selfcheck: unsupported kill-judge unit tests on synthetic unittest logs, no product mutation table
"""Unit tests of the #933 kill judge, on synthetic logs only.

The judge is the piece a forged log could otherwise fool, so every class of
forgery the fix-r2 review demonstrated gets its own case here: a marker echoed
in an ERROR traceback, a marker in another test's failure, a crash without a
unittest summary. The fix-r3 case runs main() end to end against source COPIES
with an injected final-restore failure: the persisted summary must be rewritten
INVALID, never left claiming kills whose sources were not verifiably restored.
No real source is mutated; nothing here runs cargo or node.
"""
import sys as _boot_sys
if _boot_sys.version_info < (3, 11) or 'os' not in _boot_sys.modules:
    raise SystemExit(f'#1023: os must be preloaded from CPython 3.11+ frozen modules; run with -I: python3 -I {__file__}')
import os as _boot_os

# #1023: same guard as the driver. -I keeps this directory and tests/fixtures
# out of module resolution; a reinsertion (unittest discover) is rejected
# before any local or stdlib module is imported.
# Directory identity is compared by (device, inode), never by spelling: a
# case-insensitive volume resolves a differently cased sys.path entry to this
# very directory while realpath keeps both spellings distinct, so the r1
# string comparison let a reinsertion through (fix-r2 P1-1).
_boot_dir = _boot_os.path.dirname(_boot_os.path.realpath(__file__))
# The apparatus this driver exercises is also a boot-time import
# surface: a caller can insert scripts/cli-parity and shadow a stdlib
# module before any check (GLM r4 sibling P2-2).
_boot_root = _boot_os.path.dirname(_boot_os.path.dirname(
    _boot_os.path.dirname(_boot_os.path.dirname(
        _boot_os.path.dirname(_boot_dir)))))
_boot_roots = (_boot_dir, _boot_os.path.dirname(_boot_dir),
               _boot_os.path.join(_boot_root, 'scripts', 'cli-parity'))
if not _boot_sys.flags.isolated or any(
        _boot_os.path.isdir(entry or _boot_os.curdir)
        and any(_boot_os.path.isdir(root)
                and _boot_os.path.samefile(entry or _boot_os.curdir, root)
                for root in _boot_roots)
        for entry in _boot_sys.path):
    raise SystemExit(f'#1023: unsafe import path; run with -I by file: python3 -I {__file__}')

from contextlib import contextmanager, nullcontext
import fcntl
import hashlib
import importlib.util
import json
from pathlib import Path
import shutil
import socket
import subprocess
import sys
import tempfile
import threading
import time
import unittest
from unittest.mock import patch

# Load the sibling driver by explicit absolute path, then bind its names. The
# guard above keeps a same-named module beside this file from ever winning.
_boot_driver_path = Path(__file__).resolve().with_name('negative_controls.py')
_boot_spec = importlib.util.spec_from_file_location('negative_controls', _boot_driver_path)
_boot_driver = importlib.util.module_from_spec(_boot_spec)
_boot_spec.loader.exec_module(_boot_driver)
negative_controls = _boot_driver
bound_failure_bodies = _boot_driver.bound_failure_bodies
classify = _boot_driver.classify
unittest_blocks = _boot_driver.unittest_blocks

TEST = 'ContractTests.test_occupied_mock_port_blocks_round'
MARKER = '#933-5: an occupied 28999 must make the owned mock exit and Block the round'

KILLED_LOG = """F
======================================================================
FAIL: test_occupied_mock_port_blocks_round (test_cli_parity.ContractTests.test_occupied_mock_port_blocks_round)
----------------------------------------------------------------------
Traceback (most recent call last):
  File "scripts/cli-parity/test_cli_parity.py", line 10, in test_occupied_mock_port_blocks_round
    self.assertIn('exited before listening', str(error))
AssertionError: 'exited before listening' not found in 'mock gateway did not confirm listening within 10s' : #933-5: an occupied 28999 must make the owned mock exit and Block the round

----------------------------------------------------------------------
Ran 1 test in 0.123s

FAILED (failures=1)
"""

# The forged log from the review: the marker is only the source echo of an
# ERROR block; a "marker appears somewhere" judge would count this as a kill.
ERROR_ECHO_LOG = """E
======================================================================
ERROR: test_occupied_mock_port_blocks_round (test_cli_parity.ContractTests.test_occupied_mock_port_blocks_round)
----------------------------------------------------------------------
Traceback (most recent call last):
  File "scripts/cli-parity/test_cli_parity.py", line 42, in test_occupied_mock_port_blocks_round
    self.fail('#933-5: an occupied 28999 must make the owned mock exit and Block the round')
    ^^^^^^^^^^^^^
KeyError: 'mockIsolation'

----------------------------------------------------------------------
Ran 1 test in 0.123s

FAILED (errors=1)
"""

# The bound test fails for an unrelated reason; the marker only shows up in a
# plain stdout line another test printed.
MARKER_ELSEWHERE_LOG = """marker check: #933-5: an occupied 28999 must make the owned mock exit and Block the round
F
======================================================================
FAIL: test_occupied_mock_port_blocks_round (test_cli_parity.ContractTests.test_occupied_mock_port_blocks_round)
----------------------------------------------------------------------
Traceback (most recent call last):
  File "scripts/cli-parity/test_cli_parity.py", line 10, in test_occupied_mock_port_blocks_round
    self.assertIn('x', y)
AssertionError: 'x' not found in 'y'

----------------------------------------------------------------------
Ran 1 test in 0.123s

FAILED (failures=1)
"""

# A different test carries the exact marker; the bound test is green or fails
# for its own unrelated reason.
OTHER_TEST_LOG = """F
======================================================================
FAIL: test_gateway_something_else (test_cli_parity.ContractTests.test_gateway_something_else)
----------------------------------------------------------------------
Traceback (most recent call last):
  File "scripts/cli-parity/test_cli_parity.py", line 10, in test_gateway_something_else
    self.assertIn('x', y)
AssertionError: #933-5: an occupied 28999 must make the owned mock exit and Block the round

----------------------------------------------------------------------
Ran 1 test in 0.123s

FAILED (failures=1)
"""

CRASH_LOG = """Traceback (most recent call last):
  File "scripts/cli-parity/test_cli_parity.py", line 1, in <module>
    import nothing
ModuleNotFoundError: No module named 'nothing'
"""

OK_LOG = """.
----------------------------------------------------------------------
Ran 1 test in 0.001s

OK
"""

# assertEqual puts long diffs before the message, on continuation lines of
# the exception: the marker lands after a newline, not on the AssertionError
# line itself. It is still the assertion's own message.
DIFF_CONTINUATION_LOG = """F
======================================================================
FAIL: test_mock_scenario (__main__.ContractTests.test_mock_scenario) (side='ts')
----------------------------------------------------------------------
Traceback (most recent call last):
  File "scripts/cli-parity/test_cli_parity.py", line 980, in test_mock_scenario
    self.assertEqual(compare(source, target, expected)['status'], 'PASS',
AssertionError: 'FAIL' != 'PASS'
- FAIL
+ PASS
 : #933-3: the untampered round must compare PASS; a broken profile binding rejects valid evidence

----------------------------------------------------------------------
Ran 1 test in 0.123s

FAILED (failures=1)
"""

# A subTest failure: the description follows the parenthesized qualname (the
# exact format of the Python this suite runs under; older Pythons used
# [description] and a bare class in the parentheses — both tolerated below).
SUBTEST_LOG = """F
======================================================================
FAIL: test_gateway_probe (__main__.ContractTests.test_gateway_probe) (probe='allowed')
----------------------------------------------------------------------
Traceback (most recent call last):
  File "scripts/cli-parity/test_cli_parity.py", line 10, in test_gateway_probe
    self.fail('#933-4:')
AssertionError: #933-4: probe tampering not detected

----------------------------------------------------------------------
Ran 1 test in 0.123s

FAILED (failures=1)
"""

# What the marker-truthy mutant really produces: some subTests fail at the
# pinned assertion while a sibling subTest errors out deeper in the apparatus.
# The run still completes with the standard failure exit code and a summary,
# so the pinned assertion firing is a kill; the sibling ERROR does not negate
# it (it is not an abnormal exit).
FAIL_PLUS_ERROR_LOG = """FE
======================================================================
ERROR: test_gateway_marker (__main__.ContractTests.test_gateway_marker) (bad='string')
----------------------------------------------------------------------
Traceback (most recent call last):
  File "scripts/cli-parity/test_cli_parity.py", line 42, in test_gateway_marker
    a, ac, ae = load_round(Path(left))
    ^^^^^^^^^^^^^
ValueError: not enough values to unpack (expected 3, got 0)

======================================================================
FAIL: test_gateway_marker (__main__.ContractTests.test_gateway_marker) (bad='yes')
----------------------------------------------------------------------
Traceback (most recent call last):
  File "scripts/cli-parity/test_cli_parity.py", line 12, in test_gateway_marker
    self.fail(f'#933-1: marker {bad!r} must be refused, not accepted')
AssertionError: #933-1: marker 'yes' must be refused, not accepted

----------------------------------------------------------------------
Ran 1 test in 0.123s

FAILED (failures=1, errors=1)
"""

# Older Python: the class alone in the parentheses, the method before them.
OLD_FORMAT_LOG = """F
======================================================================
FAIL: test_ping_sample (test_cli_parity.ContractTests)
----------------------------------------------------------------------
Traceback (most recent call last):
  File "scripts/cli-parity/test_cli_parity.py", line 10, in test_ping_sample
    self.assertEqual(frame, pong)
AssertionError: #933-7: the mock closed the connection during the ping exchange

----------------------------------------------------------------------
Ran 1 test in 0.123s

FAILED (failures=1)
"""


def run(returncode, stdout, stderr=''):
    return subprocess.CompletedProcess(['judge-sample'], returncode, stdout, stderr)


class UnittestBlocksTests(unittest.TestCase):
    def test_killed_log_parses_the_fail_block_with_the_assertion(self):
        blocks = unittest_blocks(KILLED_LOG)
        self.assertEqual(len(blocks), 1)
        kind, method, qualname, body = blocks[0]
        self.assertEqual((kind, method, qualname),
                         ('FAIL', 'test_occupied_mock_port_blocks_round',
                          'test_cli_parity.ContractTests.test_occupied_mock_port_blocks_round'))
        self.assertIn('AssertionError: ', '\n'.join(body))

    def test_subtest_description_is_tolerated_after_the_qualname(self):
        blocks = unittest_blocks(SUBTEST_LOG)
        self.assertEqual([block[2] for block in blocks],
                         ['__main__.ContractTests.test_gateway_probe'])
        bodies = bound_failure_bodies(blocks, 'ContractTests.test_gateway_probe')
        self.assertEqual(len(bodies), 1)
        self.assertIn('#933-4:', '\n'.join(bodies[0]))

    def test_short_description_before_the_traceback_is_skipped(self):
        described = KILLED_LOG.replace(
            '----------------------------------------------------------------------\n'
            'Traceback',
            'fix-r5: a verbose short description.\n'
            '----------------------------------------------------------------------\n'
            'Traceback', 1)
        self.assertNotEqual(described, KILLED_LOG)
        blocks = unittest_blocks(described)
        self.assertEqual(len(blocks), 1)
        self.assertIn('AssertionError: ', '\n'.join(blocks[0][3]))

    def test_bound_bodies_reject_a_lookalike_test(self):
        bodies = bound_failure_bodies(unittest_blocks(OTHER_TEST_LOG),
                                      'ContractTests.test_occupied_mock_port_blocks_round')
        self.assertEqual(bodies, [])


class ClassifyTests(unittest.TestCase):
    def test_bound_test_failed_at_its_own_assertion_is_killed(self):
        status, reason = classify(run(1, KILLED_LOG), TEST, MARKER)
        self.assertEqual(status, 'KILLED', reason)

    def test_still_passing_test_is_survived(self):
        status, _reason = classify(run(0, OK_LOG), TEST, MARKER)
        self.assertEqual(status, 'SURVIVED')

    def test_error_block_with_a_marker_echo_is_invalid(self):
        status, reason = classify(run(1, ERROR_ECHO_LOG), TEST, MARKER)
        self.assertEqual(status, 'INVALID', reason)
        self.assertIn('no FAIL block', reason)

    def test_marker_on_a_diff_continuation_line_is_killed(self):
        status, reason = classify(run(1, DIFF_CONTINUATION_LOG),
                                  'ContractTests.test_mock_scenario', '#933-3:')
        self.assertEqual(status, 'KILLED', reason)

    def test_sibling_error_subtest_does_not_negate_the_kill(self):
        status, reason = classify(run(1, FAIL_PLUS_ERROR_LOG),
                                  'ContractTests.test_gateway_marker', '#933-1:')
        self.assertEqual(status, 'KILLED', reason)

    def test_older_python_qualname_format_is_tolerated(self):
        status, reason = classify(run(1, OLD_FORMAT_LOG),
                                  'ContractTests.test_ping_sample',
                                  '#933-7: the mock closed')
        self.assertEqual(status, 'KILLED', reason)

    def test_marker_only_outside_the_bound_block_is_invalid(self):
        status, reason = classify(run(1, MARKER_ELSEWHERE_LOG), TEST, MARKER)
        self.assertEqual(status, 'INVALID', reason)
        self.assertIn('wrong failure point', reason)

    def test_marker_in_another_test_is_invalid(self):
        status, reason = classify(run(1, OTHER_TEST_LOG), TEST, MARKER)
        self.assertEqual(status, 'INVALID', reason)
        self.assertIn('no FAIL block', reason)

    def test_crash_without_a_unittest_summary_is_invalid(self):
        status, reason = classify(run(1, CRASH_LOG), TEST, MARKER)
        self.assertEqual(status, 'INVALID', reason)
        self.assertIn('no unittest summary', reason)

    def test_signal_death_is_not_a_kill(self):
        status, reason = classify(run(-11, KILLED_LOG), TEST, MARKER)
        self.assertEqual(status, 'INVALID', reason)
        self.assertIn('unexpected exit code', reason)

    def test_subtest_failures_of_the_bound_test_can_be_killed(self):
        status, reason = classify(run(1, SUBTEST_LOG),
                                  'ContractTests.test_gateway_probe', '#933-4:')
        self.assertEqual(status, 'KILLED', reason)

    def test_alternative_fragments_are_all_accepted(self):
        alternatives = ('#933-7: a pong', '#933-7: the mock closed')
        self.assertEqual(classify(run(1, KILLED_LOG), TEST, alternatives)[0], 'INVALID')
        closed = KILLED_LOG.replace(MARKER, '#933-7: the mock closed the connection during the ping exchange')
        closed = closed.replace('test_occupied_mock_port_blocks_round', 'test_ping_sample')
        status, reason = classify(run(1, closed), 'ContractTests.test_ping_sample', alternatives)
        self.assertEqual(status, 'KILLED', reason)


class RestoreFailureTests(unittest.TestCase):
    """fix-r3 P1-3: the persisted summary is only trustworthy after a clean exit."""

    def test_final_restore_failure_invalidates_the_whole_summary(self):
        # The review's P1: every mutant finished (rows flushed as KILLED),
        # then mutating_sources raised on exit — and summary.json kept saying
        # KILLED while only the exit code told the truth. Run one real table
        # row against source COPIES with run_owned faked and the context exit
        # failing, and require the persisted rows to be rewritten INVALID.
        # fix-r4 P1-2: every file main() hands to mutating_sources gets its
        # own copy — isolation_probe.mjs included (the sixth since fix-r3) —
        # and every write must land inside the copy directory: the fake
        # context's write-back is exactly where an unpatched real path would
        # be rewritten outside the real source guard's lock.
        name, _path, old, new = negative_controls.mutations[0]
        assert name == 'allow-rule-drops-port'
        test = negative_controls.tests[name]
        fragment = negative_controls.failure_fragments[name]
        sources = (negative_controls.policy, negative_controls.common, negative_controls.compare,
                   negative_controls.run_round, negative_controls.mock_gateway,
                   negative_controls.isolation_probe, negative_controls.fixture)
        # The real probe source must survive this test byte-for-byte even if
        # the copy wiring regresses, so pin it before anything is patched.
        real_probe = negative_controls.isolation_probe
        real_probe_sha256 = hashlib.sha256(real_probe.read_bytes()).hexdigest()
        tmp = Path(tempfile.mkdtemp(prefix='negctl-restore-'))
        self.addCleanup(shutil.rmtree, tmp, ignore_errors=True)
        copies = [tmp / source.name for source in sources]
        for source, copy in zip(sources, copies):
            copy.write_bytes(source.read_bytes())
        pristine = copies[0].read_bytes()
        written = []

        @contextmanager
        def fake_mutating_sources(_root, paths, *, baseline='HEAD'):
            snapshots = {path: path.read_bytes() for path in paths}
            try:
                yield snapshots
            finally:
                for path, data in snapshots.items():
                    path.write_bytes(data)
                    written.append(path)
                raise RuntimeError('injected final restore verification failure')

        fail_log = """F
======================================================================
FAIL: test_mock_profile_appends_only_the_loopback_allow (__main__.ContractTests.test_mock_profile_appends_only_the_loopback_allow) (side='ts', mock=True)
----------------------------------------------------------------------
Traceback (most recent call last):
  File "scripts/cli-parity/test_cli_parity.py", line 947, in test_mock_profile_appends_only_the_loopback_allow
    self.assertEqual(
AssertionError: 'fac4f3aaeb764a89f76d8bcc58086dd16a44d89ec873d62e1d5ff60e5af704c3' != '0f1e2d3c4b5a69788695a4b3c2d1e0f9a8b7c6d5e4f30291a8b7c6d5e4f30291a' : #933-2: the mock TS profile changed

----------------------------------------------------------------------
Ran 1 test in 0.123s

FAILED (failures=1)
"""

        def fake_run_owned(command, **_kwargs):
            # The mutated copy is on disk exactly during the negative run.
            if new.encode() in copies[0].read_bytes():
                return subprocess.CompletedProcess(command, 1, fail_log, '')
            return subprocess.CompletedProcess(command, 0, OK_LOG, '')

        out = tmp / 'evidence'
        with patch.object(negative_controls, 'mutations', [(name, copies[0], old, new)]), \
             patch.object(negative_controls, 'policy', copies[0]), \
             patch.object(negative_controls, 'common', copies[1]), \
             patch.object(negative_controls, 'compare', copies[2]), \
             patch.object(negative_controls, 'run_round', copies[3]), \
             patch.object(negative_controls, 'mock_gateway', copies[4]), \
             patch.object(negative_controls, 'isolation_probe', copies[5]), \
             patch.object(negative_controls, 'fixture', copies[6]), \
             patch.object(negative_controls, 'mutating_sources', fake_mutating_sources), \
             patch.object(negative_controls, 'machine_parity_lock', nullcontext), \
             patch.object(negative_controls, 'run_owned', fake_run_owned), \
             patch.object(sys, 'argv', ['negative_controls.py', str(out), name]):
            with self.assertRaises(SystemExit) as caught:
                negative_controls.main()
        self.assertEqual(caught.exception.code, 1,
                         'a restore failure must exit nonzero, never pass')
        # fix-r4 P1-2: nothing may write a real source — every mutating_sources
        # write-back target must be a copy under the test's own directory.
        self.assertTrue(written, 'the fake context must actually exercise its write-back')
        for path in written:
            self.assertTrue(path.is_relative_to(tmp),
                            f'a real source was written outside the copies: {path}')
        self.assertIn(copies[5], written,
                      'the sixth source copy (isolation_probe) must participate in the write-back')
        self.assertIn(copies[6], written,
                      'the seventh source copy (this driver) must participate in the write-back')
        self.assertEqual(hashlib.sha256(real_probe.read_bytes()).hexdigest(), real_probe_sha256,
                         'the real isolation_probe.mjs must be byte-identical after the test')
        summary = json.loads((out / 'summary.json').read_text())
        self.assertEqual(summary['counts'], {'KILLED': 0, 'SURVIVED': 0, 'INVALID': 1},
                         'the flushed KILLED row must be rewritten INVALID after the restore failure')
        self.assertEqual([row['status'] for row in summary['controls']], ['INVALID'],
                         'no row may stay KILLED when the sources were not verifiably restored')
        self.assertIn('source guard', summary['controls'][0]['reason'])
        self.assertIn('injected final restore verification failure', summary['controls'][0]['reason'],
                      'the persisted reason must name the restore failure')
        self.assertEqual(copies[0].read_bytes(), pristine,
                         'the source copy must end up byte-identical to its original')
        # The fragment above must really be what the faked run is judged by.
        self.assertEqual(classify(run(1, fail_log), test, fragment)[0], 'KILLED')


class IsolationGuardTests(unittest.TestCase):
    """#1023: the fixture rejects discover/reinsertion before importing."""

    here = Path(__file__).resolve().parent

    def entry(self, name):
        return self.here / name

    def run_reinserted(self, name, path_dir, entry_dir=None):
        # The shape unittest discover produces: the start directory is inserted
        # into sys.path and the module is loaded from a path. The two spellings
        # are separate on purpose: the remaining bypass only shows when sys.path
        # and ``__file__`` name the same directory differently.
        entry_dir = path_dir if entry_dir is None else entry_dir
        probe = ('import runpy, sys\n'
                 'sys.path.insert(0, sys.argv[1])\n'
                 'runpy.run_path(sys.argv[2], run_name="__main__")\n')
        return subprocess.run(
            [sys.executable, '-I', '-c', probe, str(path_dir),
             str(Path(entry_dir) / name), '--help'],
            capture_output=True, text=True, timeout=30)

    @staticmethod
    def case_alias(directory):
        """Another spelling of the same directory when the volume ignores case.

        Default APFS resolves ``FIXTURE`` to ``fixture``, so the alias names
        the same inode while realpath keeps the two spellings distinct. On a
        case-sensitive volume the alias does not exist: return None and skip
        instead of pretending the case family was exercised.
        """
        directory = Path(directory)
        alias = directory.with_name(directory.name.upper())
        if alias == directory or not alias.is_dir():
            return None
        try:
            if not _boot_os.path.samefile(alias, directory):
                return None
        except OSError:
            return None
        return alias

    def deep_mirror(self, temp):
        """A mirror deep enough for the driver's parents[5] root under /tmp.

        ``/tmp/negctl-x/fixture`` has four parents; the Linux path made
        ``parents[5]`` raise IndexError before argparse could print --help
        (fix-r1 boundary P2-3). The extra levels are inert but keep both the
        current and the legacy loader evaluable on every platform.
        """
        holder = Path(temp) / 'a' / 'b' / 'c' / 'd'
        mirror = holder / 'fixture'
        mirror.mkdir(parents=True)
        for name in ('negative_controls.py', 'test_negative_controls.py'):
            shutil.copy2(self.entry(name), mirror / name)
        shutil.copy2(self.here.parent / 'negative_process.py', holder / 'negative_process.py')
        return holder, mirror

    def disable_guard(self, mirror, marker):
        old = ("    raise SystemExit(f'#1023: unsafe import path; run with -I by file: "
               "python3 -I {__file__}')")
        new = f'    pass  # {marker} control: guard disabled'
        for name in ('negative_controls.py', 'test_negative_controls.py'):
            path = mirror / name
            text = path.read_text()
            self.assertIn(old, text, f'{marker}: {name}: the guard anchor is stale')
            path.write_text(text.replace(old, new, 1))

    def test_driver_and_test_reject_a_reinserted_fixture_directory(self):
        # Every case names the same directory; only the spelling differs. The
        # "mixed" rows are the r1 bypass: a raw/realpath string comparison saw
        # two different strings for one directory (fix-r2 P1-1).
        path_cases = [('plain path', self.here, self.here)]
        slash = Path('/' + str(self.here))
        self.assertTrue(slash.is_dir(),
                        '#1023-5: the POSIX double-slash alias must resolve')
        path_cases.append(('double-slash alias', slash, slash))
        path_cases.append(('mixed double-slash sys.path', slash, self.here))
        path_cases.append(('mixed double-slash entry', self.here, slash))
        case = self.case_alias(self.here)
        if case is not None:
            # A case-sensitive volume cannot spell this alias at all, so the
            # family is skipped there; the plain/symlink/mixed rows still run.
            path_cases.append(('mixed case sys.path', case, self.here))
            path_cases.append(('mixed case entry', self.here, case))
        with tempfile.TemporaryDirectory(prefix='negctl-1023-') as temp:
            link = Path(temp) / 'linked-fixture'
            link.symlink_to(self.here, target_is_directory=True)
            path_cases.append(('symlinked fixture dir', link, link))
            path_cases.append(('mixed symlink sys.path', link, self.here))
            path_cases.append(('mixed symlink entry', self.here, link))
            for label, path_dir, entry_dir in path_cases:
                for name in ('negative_controls.py', 'test_negative_controls.py'):
                    with self.subTest(entry=name, path=label):
                        result = self.run_reinserted(name, path_dir, entry_dir)
                        self.assertEqual(
                            result.returncode, 1,
                            f'#1023-5: {name} via {label}: a reinserted fixture dir must be rejected')
                        self.assertIn(
                            '#1023: unsafe import path', result.stderr,
                            f'#1023-5: {name} via {label}: the guard itself must reject, '
                            'not a later import error')
            # Paired control: with the guard's raise disabled, the same probe —
            # including the mixed case spelling — must start. That proves the
            # assertions above measure the guard, not an unrelated runpy/argparse
            # failure, and that the mixed spelling really reaches the directory.
            _holder, control = self.deep_mirror(temp)
            self.disable_guard(control, '#1023-5')
            control_cases = [(control, control)]
            control_case = self.case_alias(control)
            if control_case is not None:
                control_cases.append((control_case, control))
                control_cases.append((control, control_case))
            for name in ('negative_controls.py', 'test_negative_controls.py'):
                for path_dir, entry_dir in control_cases:
                    with self.subTest(entry=name, control='guard disabled',
                                      spelling=str(path_dir)):
                        result = self.run_reinserted(name, path_dir, entry_dir)
                        self.assertEqual(
                            result.returncode, 0,
                            f'#1023-5 control: removing the guard must let the reinserted {name} '
                            f'start: {result.stderr}')

    def run_nonisolated(self, entry):
        # A direct script launch puts the script directory on sys.path, so the
        # reinsertion branch would reject even with the isolation flag check
        # removed. ``-c`` + runpy keeps the fixture out of sys.path, making the
        # ``not sys.flags.isolated`` branch the only rejection under test.
        probe = ('import runpy, sys\n'
                 'path = sys.argv[1]\n'
                 'sys.argv = [path, "--help"]\n'
                 'runpy.run_path(path, run_name="__main__")\n')
        return subprocess.run(
            [sys.executable, '-E', '-s', '-c', probe, str(entry)],
            cwd=self.here.parents[5], capture_output=True, text=True, timeout=30)

    def test_driver_rejects_a_nonisolated_launch(self):
        result = self.run_nonisolated(self.entry('negative_controls.py'))
        self.assertEqual(result.returncode, 1, '#1023-6: without -I the entry must exit at the guard')
        self.assertIn('#1023: unsafe import path', result.stderr,
                      '#1023-6: the non-isolated launch must be named by the guard')
        with tempfile.TemporaryDirectory(prefix='negctl-1023-') as temp:
            _holder, control = self.deep_mirror(temp)
            self.disable_guard(control, '#1023-6')
            result = self.run_nonisolated(control / 'negative_controls.py')
            self.assertEqual(result.returncode, 0,
                             '#1023-6 control: removing the guard must let a non-isolated launch '
                             'start: ' + result.stderr)

    def test_explicit_load_does_not_reinsert_the_parent_directory(self):
        # The old loader inserted parents[1] into sys.path; a planted
        # subprocess.py there then won inside the shared helper. The new loader
        # opens that helper by path and leaves sys.path untouched.
        with tempfile.TemporaryDirectory(prefix='negctl-1023-') as temp:
            holder, mirror = self.deep_mirror(temp)
            marker = holder / 'IMPOSTER_EXECUTED'
            # ``subprocess`` is imported by the shared helper, i.e. after the
            # legacy loader's sys.path.insert; planting json.py here would be
            # inert because the driver imports json before the loader.
            (holder / 'subprocess.py').write_text(
                f'open({str(marker)!r}, "w").write("imposter")\n')
            result = subprocess.run(
                [sys.executable, '-I', str(mirror / 'negative_controls.py'), '--help'],
                capture_output=True, text=True, timeout=30)
            self.assertEqual(result.returncode, 0,
                             '#1023-7: the driver must load its real sibling helper: ' + result.stderr)
            self.assertFalse(marker.exists(),
                             '#1023-7: the parent directory must not be reinserted into sys.path')
            # Paired control: splice the legacy ``sys.path.insert`` loader into
            # a second copy. The planted subprocess.py must then execute, proving the
            # assertion above can fail when the explicit load is reverted.
            legacy_holder = Path(temp) / 'legacy' / 'a' / 'b' / 'c' / 'd'
            legacy_mirror = legacy_holder / 'fixture'
            legacy_mirror.mkdir(parents=True)
            source = (mirror / 'negative_controls.py').read_text()
            start = source.index('_boot_module_path = ')
            end = source.index('sys.meta_path.remove(_boot_finder)')
            end = source.index('\n', end) + 1
            legacy = ('sys.path.insert(0, str(Path(__file__).resolve().parents[1]))\n'
                      'from negative_process import mutating_sources, run_owned  # noqa: E402\n')
            (legacy_mirror / 'negative_controls.py').write_text(
                source[:start] + legacy + source[end:])
            shutil.copy2(self.here.parent / 'negative_process.py',
                         legacy_holder / 'negative_process.py')
            legacy_marker = legacy_holder / 'IMPOSTER_EXECUTED'
            (legacy_holder / 'subprocess.py').write_text(
                f'open({str(legacy_marker)!r}, "w").write("imposter")\n')
            result = subprocess.run(
                [sys.executable, '-I', str(legacy_mirror / 'negative_controls.py'), '--help'],
                capture_output=True, text=True, timeout=30)
            self.assertTrue(
                legacy_marker.exists(),
                '#1023-7 control invalid: the legacy sys.path loader did not execute the '
                f'planted subprocess.py: {result.stderr}')

    def test_port_preflight_waits_for_a_released_listener(self):
        listener = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
        listener.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        listener.bind(('127.0.0.1', 0))
        listener.listen(1)
        port = listener.getsockname()[1]
        try:
            with self.assertRaises(RuntimeError,
                                   msg='#1023-8: an occupied port must block the run'):
                negative_controls.wait_for_port_free(port, timeout=0.2)
        finally:
            listener.close()
        self.assertIsNone(negative_controls.wait_for_port_free(port, timeout=2),
                          '#1023-8: a released port must be admitted')


class MachineLeaseTests(unittest.TestCase):
    """#1023 fix-r2 concurrency P1: a bound run holds the shared machine lease."""

    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix='negctl-lease-')
        self.addCleanup(self.temp.cleanup)
        self.lock = Path(self.temp.name) / 'parity.lock'

    def test_bound_run_lease_times_out_on_a_stale_owner(self):
        # The predecessor never finishes: the wait is bounded, and the row must
        # become a named INVALID rather than run into a foreign bind.
        with negative_controls.lease_module().parity_lock(self.lock):
            with self.assertRaises(
                    RuntimeError,
                    msg='#1023-9: a held lease must never admit the bound run'):
                with negative_controls.machine_parity_lock(
                        timeout=0.2, interval=0.02, lock=self.lock):
                    self.fail('#1023-9: the bound run started while the lease was held')

    def test_bound_run_lease_is_held_then_released(self):
        with negative_controls.machine_parity_lock(
                timeout=2, interval=0.02, lock=self.lock):
            self.assertTrue(self.lock.is_dir(),
                            '#1023-9: the lease must be held for the whole child run')
        self.assertFalse(self.lock.exists(),
                         '#1023-9: a finished bound run must release the lease')

    @staticmethod
    def _stack_names(thread):
        """Function names on a live thread's stack (caller must be running)."""
        frame = sys._current_frames().get(thread.ident)
        names = []
        while frame is not None:
            names.append(frame.f_code.co_name)
            frame = frame.f_back
        return names

    def _wait_until_competing(self, thread, marker):
        """Wait until the caller is inside parity_lock's acquisition.

        ``deadline_set`` already proved the deadline exists; this proves the
        caller actually started competing for the lease before the holder may
        release it (fix-r5 Codex P1-2: the old timer could fire before the
        deadline was even computed).
        """
        deadline = time.monotonic() + 5
        while time.monotonic() < deadline:
            names = self._stack_names(thread)
            if 'machine_parity_lock' in names and 'parity_lock' in names:
                return True
            time.sleep(0.001)
        return False

    def test_bound_run_lease_late_arrival_does_not_start_bound_run(self):
        marker = ('#1023-9: a lease released after the deadline must not start '
                  'the bound run')
        guard_path = self.lock.with_name(self.lock.name + '.guard')
        acquired = threading.Event()
        release = threading.Event()
        started = []
        errors = []

        class Clock:
            """Deterministic monotonic clock for machine_parity_lock only.

            The first read establishes the deadline at 0.0; every later read
            is past it, so the post-acquisition re-check necessarily sees a
            late lease. A real timer could fire before the caller established
            its deadline, which is the false failure Codex P1-2 reported.
            """
            def __init__(self):
                self.reads = 0
                self.deadline_set = threading.Event()

            def monotonic(self):
                self.reads += 1
                if self.reads == 1:
                    self.deadline_set.set()
                    return 0.0
                return 1000.0

            def sleep(self, seconds):
                raise AssertionError(
                    marker + ': the late arrival must block in flock, not poll')

        clock = Clock()

        def holder():
            # Hold only the .guard flock, without creating the lease directory:
            # this is the window in which parity_lock's flock blocks past the
            # deadline instead of raising Blocked on an existing directory.
            with open(guard_path, 'w+') as guard:
                fcntl.flock(guard, fcntl.LOCK_EX)
                acquired.set()
                release.wait(5)

        def caller():
            try:
                with negative_controls.machine_parity_lock(
                        timeout=0.05, interval=0.005, lock=self.lock):
                    started.append(True)
            except BaseException as error:
                errors.append(error)

        original_time = negative_controls.time
        thread = threading.Thread(target=holder, daemon=True)
        caller_thread = threading.Thread(target=caller, daemon=True)
        thread.start()
        try:
            self.assertTrue(acquired.wait(5),
                            marker + ': the sibling never acquired the lease')
            negative_controls.time = clock
            caller_thread.start()
            self.assertTrue(clock.deadline_set.wait(5),
                            marker + ': the caller never established its deadline')
            self.assertTrue(
                self._wait_until_competing(caller_thread, marker),
                marker + ': the caller never started competing for the lease')
            release.set()
            caller_thread.join(5)
            self.assertFalse(caller_thread.is_alive(),
                             marker + ': the caller did not return after the release')
        finally:
            release.set()
            caller_thread.join(5)
            negative_controls.time = original_time
            thread.join(5)
        self.assertEqual(len(errors), 1,
                         marker + f': the late lease must raise once, got {errors!r}')
        self.assertIsInstance(errors[0], RuntimeError,
                              marker + f': wrong rejection: {errors[0]!r}')
        self.assertIn('released after the', str(errors[0]),
                      marker + f': wrong rejection: {errors[0]}')
        self.assertEqual(started, [], marker + ': no bound run may start')
        self.assertFalse(self.lock.exists(), marker + ': a late lease must be released')

    def test_bound_run_lease_blocks_on_a_live_guard_then_proceeds(self):
        """Real flock: a live guard blocks the caller, release lets it proceed."""
        marker = '#1023-9: a live guard must block the caller until it is released'
        guard_path = self.lock.with_name(self.lock.name + '.guard')
        acquired = threading.Event()
        release = threading.Event()
        started = []
        errors = []

        def holder():
            with open(guard_path, 'w+') as guard:
                fcntl.flock(guard, fcntl.LOCK_EX)
                acquired.set()
                # Only release.set() may drop the flock (the finally below
                # always sets it): a self-timeout here would silently change
                # the timing under test (#1023 r5 Codex P1-1).
                release.wait()

        def caller():
            try:
                with negative_controls.machine_parity_lock(
                        timeout=5, interval=0.005, lock=self.lock):
                    started.append(True)
            except BaseException as error:
                errors.append(error)

        thread = threading.Thread(target=holder, daemon=True)
        caller_thread = threading.Thread(target=caller, daemon=True)
        thread.start()
        try:
            self.assertTrue(acquired.wait(5),
                            marker + ': the sibling never acquired the flock')
            caller_thread.start()
            self.assertTrue(
                self._wait_until_competing(caller_thread, marker),
                marker + ': the caller never reached the acquisition')
            # Still holding the flock: the caller must remain blocked for a
            # measurable interval, not merely not-yet-scheduled.
            time.sleep(0.1)
            self.assertEqual(started, [],
                             marker + ': the bound run started while the flock was held')
            self.assertEqual(errors, [],
                             marker + f': the waiting caller raised early: {errors!r}')
            release.set()
            caller_thread.join(5)
            self.assertFalse(caller_thread.is_alive(),
                             marker + ': the caller did not proceed after the release')
        finally:
            release.set()
            caller_thread.join(5)
            thread.join(5)
        self.assertEqual(errors, [], marker + f': the bound run failed: {errors!r}')
        self.assertEqual(started, [True],
                         marker + ': the released lease must admit the bound run')
        self.assertFalse(self.lock.exists(), marker + ': the lease must be released')

    def test_bound_run_lease_is_released_on_cancellation(self):
        with self.assertRaises(KeyboardInterrupt,
                               msg='#1023-9: cancellation must propagate'):
            with negative_controls.machine_parity_lock(
                    timeout=2, interval=0.02, lock=self.lock):
                raise KeyboardInterrupt('#1023-9: cancel the bound run')
        self.assertFalse(self.lock.exists(),
                         '#1023-9: cancellation must release the lease')


if __name__ == '__main__':
    unittest.main(verbosity=2)
