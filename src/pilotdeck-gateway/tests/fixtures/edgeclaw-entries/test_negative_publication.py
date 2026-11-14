# anchor-selfcheck: unsupported unit tests of the summary publication, no product source mutations
#!/usr/bin/env python3
"""#1087 fix-r5 (sibling #3) controls for the five entries runners.

Each delivered runner (``negative.py``, ``negative_fix1.py``,
``negative_ids.py``, ``negative_round6.py``, ``negative_round7.py``) keeps its
verdict rows in memory for the whole guarded window and publishes one final
``summary.json`` by a single rename only after ``mutating_sources`` restored
and byte-verified the guarded sources. An abort inside the window — a
cancellation, a restore failure — voids the recorded rows, publishes INVALID
and exits 3; a stale anchor is a refusal (exit 2, nothing published).

Every script is a top-level main, so each control executes its runner a second
time with only the process layer mocked (cargo, slots, guard, source writes);
the bookkeeping under test is the delivered code. The guarded sources stay on
disk untouched: the mocked writes record instead of writing, and the restore
verification reads the same real bytes the guard snapshotted.

Run with:

    python3 -B edgeclaw-entries/test_negative_publication.py
"""
import ast
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import types
import unittest
from unittest.mock import patch

_FIXTURES = Path(__file__).resolve().parents[1]
if str(_FIXTURES) not in sys.path:
    sys.path.insert(0, str(_FIXTURES))
import negative_process as shared_process

HERE = Path(__file__).resolve().parent


def load_negative_table():
    """Count the entries ``mutations`` rows of the delivered ``negative.py``."""
    tree = ast.parse((HERE / 'negative.py').read_text())
    for node in tree.body:
        if (isinstance(node, ast.Assign) and isinstance(node.targets[0], ast.Name)
                and node.targets[0].id == 'mutations'
                and isinstance(node.value, ast.List)):
            base = len(node.value.elts)
            # One platform row is appended after the list literal.
            return base + 1
    raise AssertionError('negative.py must keep the module-level mutations table')


ENTRIES_ROWS = load_negative_table()


class EntriesPublicationBase:
    """Shared harness: execute one delivered runner with the process layer mocked.

    Not a TestCase itself: only the five script subclasses below are
    collected, each mixing this base in with ``unittest.TestCase``.
    """

    script = None            # delivered runner file name
    extra_argv = ()          # e.g. ('--only', 'path-read')
    positional_out = False   # round7 takes a positional OUT_DIR
    restored_run_index = None  # 1-based run index of a trailing restored suite (ids)
    needs_environment = False  # round6/round7 import target_environment
    needs_published_module = False  # round6 imports published_module

    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory(prefix='entries-negctl-')
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name)
        # A scratch file the synthetic failure line points at as its replay
        # diff; the delivered runner copies its bytes into the evidence dir.
        self.diff = self.root / 'diff.txt'
        self.diff.write_text('replay diff\n')

    def argv(self, out):
        if self.positional_out:
            return [str(HERE / self.script), str(out)]
        return [str(HERE / self.script), '--out', str(out), *self.extra_argv]

    def kill_log(self):
        return ('running 1 test\ntest entries_replay ... FAILED\n'
                '---- entries_replay stdout ----\n'
                "thread 'entries_replay' panicked at tests/entries.rs:1:1:\n"
                # The judge requires 'scenario ' and 'observation ' (each with
                # its trailing space), like the real replay failure line.
                f'scenario malformed-ids observation expected-rejection diff {self.diff}\n'
                'note: run with `RUST_BACKTRACE=1` environment variable to '
                'display a backtrace\n'
                'test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; '
                '0 filtered out\n')

    def green_log(self):
        return ('running 2 tests\ntest result: ok. 2 passed; 0 failed; 0 '
                'ignored; 0 measured; 0 filtered out\n')

    def _drive(self, *, fail_at=None, fail_with=None, restore_fails_at=None,
               stale_anchors=False, kill=True, out_dir=None):
        """Execute the delivered script once with the process layer mocked.

        ``out_dir`` reuses an evidence directory a previous drive already
        filled (the delivered runners mkdir with ``exist_ok=True``).
        """
        out = (out_dir or (self.root / 'run')).resolve()
        state = {'runs': 0, 'writes': 0, 'summary_existed': False}
        disk = {}

        def fake_run_owned(command, *, env=None, cwd=None, timeout=None):
            # #1087 fix-r5: the real sample run caught a rewritten command
            # whose 'offline' had lost its dashes (cargo rejects two
            # positional TESTNAMEs), so the twin pins the flag spelling the
            # mocked process layer otherwise hides.
            assert command[:1] == ['cargo'] and '--offline' in command, command
            state['runs'] += 1
            if fail_at == state['runs']:
                raise fail_with
            if (self.restored_run_index is not None
                    and state['runs'] == self.restored_run_index):
                return subprocess.CompletedProcess(command, 0, '', self.green_log())
            if kill:
                return subprocess.CompletedProcess(command, 101, '', self.kill_log())
            return subprocess.CompletedProcess(command, 0, '', self.green_log())

        def fake_mutating(root, paths, **kwargs):
            if stale_anchors:
                originals = {path: b'fn unrelated() {}' for path in paths}
            else:
                originals = {path: path.read_bytes() for path in paths}

            class Guard:
                def __enter__(self):
                    return originals

                def __exit__(self, *exc):
                    return False

            return Guard()

        def fake_slot(script, **kwargs):
            return subprocess.CompletedProcess([str(script)], 0, '', '')

        def fake_atomic(root, path, data, *, mode=None):
            state['writes'] += 1
            state['summary_existed'] = (state['summary_existed']
                                        or bool(list(out.glob('summary.*.json'))))
            if restore_fails_at is not None and state['writes'] == restore_fails_at:
                raise RuntimeError('twin restore failure')

        def is_summary_tmp(path):
            # #1087 fix-r8 (S2-a): the staging name prefixes the per-run
            # summary name, not the fixed ``summary.json.``.
            return (path.parent == out and path.name.startswith('summary.')
                    and path.name.endswith('.tmp'))

        class Model:
            real_write_text = Path.write_text
            real_replace = Path.replace

            def model_write_text(self, data, *args, **kwargs):
                if (self.parent == out and self.name.startswith('summary.')
                        and self.name.endswith('.json')):
                    disk['direct'] = data
                    return None
                if is_summary_tmp(self):
                    disk['staged'] = data
                return Model.real_write_text(self, data, *args, **kwargs)

            def model_replace(self, destination, *args, **kwargs):
                if is_summary_tmp(self):
                    disk['renames'] = disk.get('renames', 0) + 1
                    disk['summary'] = disk['staged']
                return Model.real_replace(self, destination, *args, **kwargs)

        source = HERE / self.script
        fake = types.ModuleType('negative_process')
        fake.mutating_sources = fake_mutating
        fake.run_owned = fake_run_owned
        fake.wait_for_slot = fake_slot
        fake.write_source_atomic = fake_atomic
        # #1087 fix-r5: the shared helpers stay the shipped ones so the twins
        # exercise the real staging-name and dead-fd behavior.
        # #1087 fix-r8 (S1-a/S1-b/S1-c): the new shared originals (guarded
        # print, per-run token, refused-marker writer) are bound the same
        # way — the twins exercise the delivered code against the real ones.
        fake.unique_summary_tmp = shared_process.unique_summary_tmp
        fake.detach_dead_streams = shared_process.detach_dead_streams
        fake.dead_stream_print = shared_process.dead_stream_print
        fake.new_run_token = shared_process.new_run_token
        fake.refuse_summary = shared_process.refuse_summary
        if self.needs_environment:
            fake.target_environment = shared_process.target_environment
        if self.needs_published_module:
            fake.published_module = shared_process.published_module
        saved_process = sys.modules.get('negative_process')
        saved_argv = sys.argv
        saved_cwd = Path.cwd()
        sys.modules['negative_process'] = fake
        sys.argv = self.argv(out)
        try:
            with patch.object(Path, 'write_text', Model.model_write_text), \
                 patch.object(Path, 'replace', Model.model_replace):
                exec(compile(source.read_text(), str(source), 'exec'),
                     {'__name__': '__main__', '__file__': str(source)})
        except BaseException as error:  # the delivered script always exits
            code = error
        else:  # pragma: no cover - the script never falls off the end
            code = None
        finally:
            sys.argv = saved_argv
            if saved_process is None:
                sys.modules.pop('negative_process', None)
            else:
                sys.modules['negative_process'] = saved_process
            os.chdir(saved_cwd)
        return code, state, disk, out

    def per_run_summaries(self, out):
        """The ``summary.<token>.json`` files a run published (#1087 fix-r8 S2-a).

        The verdict file is per-run now, so a reader (this twin, a host
        coordinator) globs the shape instead of opening the fixed name.
        """
        return list(out.glob('summary.*.json'))

    # -- the shared contract, parameterized only by the row count --

    def expected_rows(self):
        raise NotImplementedError

    def test_green_run_publishes_via_one_rename(self):
        code, state, disk, out = self._drive()
        message = '#1087 the final summary must be one rename, never an in-place write'
        self.assertIsInstance(code, SystemExit,
                              message + ' (the sample run must exit cleanly)')
        self.assertTrue(code.code == 0, message + ' (the sample run must stay green)')
        self.assertTrue(disk.get('direct') is None,
                        message + ' (summary.json was written in place)')
        self.assertTrue(disk.get('renames') == 1,
                        message + ' (the summary was not published by exactly one rename)')
        payload = json.loads(disk['summary'])
        rows = payload['results']
        self.assertTrue(len(rows) == self.expected_rows(),
                        message + ' (the final payload must keep every row)')
        self.assertTrue(all(row['killed'] for row in rows),
                        message + ' (every row must record its kill)')
        self.assertTrue('restoredSha256' in payload,
                        message + ' (the sha256 map must stay in the schema)')

    def test_no_summary_is_readable_inside_the_guarded_window(self):
        code, state, disk, out = self._drive()
        message = '#1087 no summary may be readable before restore verification'
        self.assertTrue(code.code == 0, message + ' (the sample run must stay green)')
        self.assertTrue(state['summary_existed'] is False,
                        message + ' (a summary was readable inside the guarded window)')
        self.assertTrue(not list(out.glob('*.tmp')),
                        message + ' (a temp file was left behind)')

    def test_keyboardinterrupt_voids_rows_and_propagates(self):
        code, state, disk, out = self._drive(
            fail_at=1, fail_with=KeyboardInterrupt('red run interrupted'))
        message = '#1087 a cancellation must void the rows and publish INVALID'
        self.assertIsInstance(code, KeyboardInterrupt,
                              message + ' (the cancellation must be re-raised)')
        # #1087 fix-r8 (S2-a): the voided verdict lives at this run's
        # per-run summary name; the glob finds exactly that one file.
        summaries = self.per_run_summaries(out)
        self.assertTrue(len(summaries) == 1,
                        message + ' (the aborted run must publish exactly one summary)')
        payload = json.loads(summaries[0].read_text())
        self.assertTrue(len(payload['results']) == self.expected_rows(),
                        message + ' (the unstarted mutants must be recorded too)')
        self.assertTrue(all(row['killed'] is False for row in payload['results']),
                        message + ' (the on-disk rows were not voided)')
        self.assertTrue(
            all('runner: KeyboardInterrupt' in row['voided']
                for row in payload['results']),
            message + ' (the invalidation reason was not recorded)')

    def test_restore_failure_voids_rows_and_exits_three(self):
        code, state, disk, out = self._drive(restore_fails_at=2)
        message = '#1087 a source-write failure must void the rows and exit 3'
        self.assertIsInstance(code, SystemExit,
                              message + ' (the failure must exit through SystemExit)')
        self.assertTrue(code.code == 3, message + ' (a source-write failure must exit 3)')
        summaries = self.per_run_summaries(out)
        self.assertTrue(len(summaries) == 1,
                        message + ' (the aborted run must publish exactly one summary)')
        payload = json.loads(summaries[0].read_text())
        self.assertTrue(len(payload['results']) == self.expected_rows(),
                        message + ' (the unstarted mutants must be recorded too)')
        self.assertTrue(
            all('runner: RuntimeError' in row['voided'] for row in payload['results']),
            message + ' (the invalidation reason was not recorded)')

    def test_stale_anchor_is_a_refusal_that_publishes_nothing(self):
        code, state, disk, out = self._drive(stale_anchors=True)
        message = '#1087 a stale anchor must be a never-started refusal'
        self.assertIsInstance(code, SystemExit,
                              message + ' (the refusal must exit through SystemExit)')
        self.assertTrue(code.code == 2,
                        message + ' (the stale-anchor refusal must stay exit 2)')
        # #1087 fix-r8 (S1-c + S2-a): the refusal publishes no verdict — the
        # only summary this run may leave is its own refused marker, which
        # carries the reason and never a results row.
        summaries = self.per_run_summaries(out)
        # fix-r11 (review MM sibling #1): pin the marker's existence — an
        # all() over an empty glob is vacuously true.
        self.assertTrue(len(summaries) == 1,
                        message + ' (the refusal must leave exactly its own '
                        'marker — an empty glob is not a refusal)')
        self.assertTrue(all('refused' in json.loads(path.read_text())
                            for path in summaries),
                        message + ' (a verdict summary was published for a refusal)')
        self.assertTrue(not list(out.glob('*.tmp')),
                        message + ' (a refusal must not leave staging behind)')

    def test_a_refusal_in_a_reused_directory_never_touches_the_other_run(self):
        """#1087 fix-r8 (S1-c + S2-a): a refused re-run leaves the other run alone.

        The OUT_DIR is a plain reused mkdir, so a host re-run can land in the
        same path after a green run. With the per-run summary names the
        exit-2 stale-anchor refusal writes its own refused marker under its
        own token and the previous run's green summary stays byte-identical
        — run B can no longer destroy run A's verdict (fix-r6's marker
        overwrote the shared fixed name; concurrency #4 closed that).
        """
        shared = (self.root / 'shared').resolve()
        code_a, _state_a, _disk_a, out = self._drive(out_dir=shared)
        message = '#1087 a refused re-run must not touch the other run\'s summary'
        self.assertTrue(code_a.code == 0,
                        message + ' (the first run into the directory must stay green)')
        green = self.per_run_summaries(out)
        self.assertTrue(len(green) == 1,
                        message + ' (the green run published exactly one summary)')
        green_payload = json.loads(green[0].read_text())
        self.assertTrue('results' in green_payload,
                        message + ' (the green run\'s verdict must carry its rows)')
        green_name = green[0].name
        code_b, _state_b, _disk_b, out = self._drive(out_dir=shared,
                                                      stale_anchors=True)
        self.assertIsInstance(code_b, SystemExit,
                              message + ' (the refusal must exit through SystemExit)')
        self.assertTrue(code_b.code == 2,
                        message + ' (the stale-anchor refusal must stay exit 2)')
        # The other run's green verdict is untouched — same name, same bytes.
        self.assertTrue(green[0].name == green_name,
                        message + ' (the green summary was renamed)')
        self.assertTrue(json.loads(green[0].read_text()) == green_payload,
                        message + ' (the green summary was rewritten)')
        # This run's own refused marker exists, carries the reason, and no rows.
        marker = [path for path in self.per_run_summaries(out) if path.name != green_name]
        self.assertTrue(len(marker) == 1,
                        message + ' (the refused run must leave its own marker)')
        payload = json.loads(marker[0].read_text())
        self.assertTrue('refused' in payload,
                        message + ' (the refused marker must carry the refusal)')
        self.assertTrue('stale anchors' in payload['refused'],
                        message + ' (the refused marker must carry the reason)')
        self.assertTrue('results' not in payload,
                        message + ' (the refused marker must carry no verdict rows)')
        self.assertTrue(payload['runToken'] != green_payload['runToken'],
                        message + ' (the marker must name a different run than the green one)')
        self.assertTrue(not list(out.glob('*.tmp')),
                        message + ' (the refusal must not leave staging behind)')

    def test_stale_anchor_refusal_survives_a_dead_stderr(self):
        """#1087 fix-r6 (P1b): a dead stderr must keep the refusal at exit 2.

        ``2>&-`` or a closed stream makes the STALE ANCHOR print raise; the
        old unguarded print turned the refusal into an in-window abort (exit 3
        with a voided INVALID summary published) instead of exit 2.
        """
        class ClosedStderr:
            def write(self, data):
                raise OSError('stderr is closed')

            def flush(self):
                pass

        with patch.object(sys, 'stderr', ClosedStderr()):
            code, state, disk, out = self._drive(stale_anchors=True)
        message = '#1087 a dead stderr must keep the stale-anchor refusal at exit 2'
        self.assertIsInstance(code, SystemExit,
                              message + ' (the refusal must exit through SystemExit)')
        self.assertTrue(code.code == 2,
                        message + ' (the refusal must stay exit 2, not abort 3)')
        # #1087 fix-r8 (S2-a): no verdict may be readable — the only thing a
        # refusal may leave is its own refused marker.
        marker = self.per_run_summaries(out)
        # fix-r11 (review DS sibling #3): pin the marker's existence — an
        # all() over an empty glob is vacuously true.
        self.assertTrue(len(marker) == 1,
                        message + ' (the refusal must leave its own marker — '
                        'an empty glob is not a refusal)')
        self.assertTrue(all('refused' in json.loads(path.read_text())
                            for path in marker),
                        message + ' (nothing may be published for a refusal)')

    def test_two_runs_on_one_directory_never_share_a_log_file(self):
        """#1087 fix-r6 (P1a): two runs on one reused directory keep distinct logs.

        The per-control log names carry a pid.serial run token, so two
        overlapping runs of this runner on the same reused OUT_DIR stage their
        logs separately — the old fixed ``{name}.log`` was a shared mutable
        resource one run truncated under the other.
        """
        shared = (self.root / 'shared').resolve()
        code_a, _state_a, _disk_a, out = self._drive(out_dir=shared)
        logs_a = {path.name for path in out.glob('*.log')}
        # A second, overlapping run is a different process: same serial, other
        # pid, so its token — and with it every log name — differs.
        with patch.object(os, 'getpid', lambda: 424242):
            code_b, _state_b, _disk_b, out = self._drive(out_dir=shared)
        logs_b = {path.name for path in out.glob('*.log')} - logs_a
        message = '#1087 two runs on one directory must not share a log file'
        self.assertTrue(code_a.code == 0 and code_b.code == 0,
                        message + ' (both sample runs must stay green)')
        self.assertTrue(logs_a and len(logs_b) == len(logs_a),
                        message + f' (the second run added {sorted(logs_b)} '
                        f'to {sorted(logs_a)})')
        self.assertTrue(not (logs_a & logs_b),
                        message + ' (a fixed log name was shared between the runs)')
        # #1087 fix-r8 (S1-b): the shared token carries three components
        # (pid.serial.time_ns), so the label is everything before them.
        labels_a = {name.removesuffix('.log').rsplit('.', 3)[0] for name in logs_a}
        labels_b = {name.removesuffix('.log').rsplit('.', 3)[0] for name in logs_b}
        self.assertTrue(labels_a == labels_b,
                        message + f' (the labels must keep their identity across '
                        f'the tokens; got {sorted(labels_a)} and {sorted(labels_b)})')


class NegativeMainPublication(EntriesPublicationBase, unittest.TestCase):
    script = 'negative.py'

    def expected_rows(self):
        return ENTRIES_ROWS


class NegativeFix1Publication(EntriesPublicationBase, unittest.TestCase):
    script = 'negative_fix1.py'
    extra_argv = ('--only', 'path-read')

    def expected_rows(self):
        return 1


class NegativeIdsPublication(EntriesPublicationBase, unittest.TestCase):
    script = 'negative_ids.py'
    # Ten mutation runs, then the trailing restored suite (run 11) that the
    # verdict's restoredPass flag comes from.
    restored_run_index = 11

    def expected_rows(self):
        return 10

    def test_restored_run_green_is_part_of_the_verdict(self):
        # The trailing restored suite is a verdict input, not a separate exit:
        # a red restored run keeps exit 1 even when every mutant was killed.
        code, state, disk, out = self._drive()
        summaries = self.per_run_summaries(out)
        self.assertTrue(len(summaries) == 1,
                        '#1087 the run must publish exactly one per-run summary')
        payload = json.loads(summaries[0].read_text())
        self.assertTrue(payload['restoredPass'] is True,
                        '#1087 the restored suite verdict must be published')
        self.assertTrue(payload['complete'] is True,
                        '#1087 the completeness flag must be published')
        self.assertTrue('restoredBytes' in payload,
                        '#1087 the schema must stay additive')


class NegativeRound6Publication(EntriesPublicationBase, unittest.TestCase):
    script = 'negative_round6.py'
    extra_argv = ('--only', 'global-panic')
    needs_environment = True
    needs_published_module = True

    def expected_rows(self):
        return 1


class NegativeRound7Publication(EntriesPublicationBase, unittest.TestCase):
    script = 'negative_round7.py'
    positional_out = True
    needs_environment = True

    def expected_rows(self):
        return 7


if __name__ == '__main__':
    unittest.main()
