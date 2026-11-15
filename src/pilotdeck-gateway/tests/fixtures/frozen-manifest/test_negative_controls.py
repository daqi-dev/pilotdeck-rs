# anchor-selfcheck: unsupported #3110 atomic-publish twin over a temporary source, no product source mutations
#!/usr/bin/env python3
"""#3110 (#3178 item 6): the frozen-manifest mutation publish is one rename.

The family runner used to publish its mutant with ``path.write_text(mutated)``
— a truncating in-place write on the shared checkout's real source (#3110,
opened from MM r14 concurrency#2: a kill inside the write window leaves an
empty or half-written product source behind, the #3034 /
「negctl-stale-pyc-swap-payload」 residue class). The shipped publish and
restore now go through ``negative_process.write_source_atomic`` (the #2982 /
#1903 line: a temp file plus ONE rename, the mode snapshotted by the helper
itself).

This twin is the baseline self-check of that shape: it drives the delivered
``judge_row`` in-process over a temporary source — only the bound-test
executor layer (``run_owned``) is faked; the publish, the classification and
the restore are the shipped code — and pins the write channel itself. The
mutant and the restore must each cross ``write_source_atomic`` exactly once,
the mutant must be on disk before the bound negative run observes it, and
the original bytes must survive the round trip. A revert to a truncating
``write_text`` / ``write_bytes`` keeps the bytes (that is what makes it a
silent hazard) but changes the channel — the twin goes red on its dedicated
judgment, not on an unrelated diff. No cargo, no lease, no product source
writes: the guarded path lives in a TemporaryDirectory.

Run with:

    python3 -B -m unittest -v frozen-manifest/test_negative_controls.py
"""
import importlib.util
import subprocess
import unittest
from pathlib import Path
from unittest.mock import patch


def load_runner():
    source = Path(__file__).with_name('negative_controls.py')
    spec = importlib.util.spec_from_file_location(
        'frozen_manifest_negative', source)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


class AtomicPublishControls(unittest.TestCase):
    """The mutation publish and restore are each one atomic rename (#3110)."""

    def test_publish_and_restore_cross_write_source_atomic(self):
        import tempfile
        runner = load_runner()
        with tempfile.TemporaryDirectory(prefix='pdrs-fm-twin-') as temp:
            base = Path(temp)
            guarded = base / 'check_frozen_probe.py'
            original = (b'# probe source for the #3110 twin\n'
                        b'if generated_manifest(root, raw) != raw:\n'
                        b'    raise SystemExit(1)\n')
            guarded.write_bytes(original)
            out = base / 'evidence'
            out.mkdir()
            test = 'twin_bound_test'
            row = ('twin-atomic-publish', guarded,
                   'if generated_manifest(root, raw) != raw:', 'if False:',
                   test, '#twin bound assertion must fire')
            marker = f'{test} (__main__.Manifest.{test}) ... ok'
            failure = (f'FAIL: {test} (__main__.Manifest.{test})\n'
                       'AssertionError: #twin bound assertion must fire\n'
                       'FAILED (failures=1)\n')
            mutated = original.replace(row[2].encode(), row[3].encode(), 1)
            channel = []
            observed = []
            stage = {'call': 0}
            real_atomic = runner.write_source_atomic

            def recording_atomic(root_arg, path, data, *, mode=None):
                channel.append((Path(path), bytes(data)))
                return real_atomic(root_arg, path, data, mode=mode)

            def fake_run_owned(command, cwd=None, timeout=None, **kwargs):
                stage['call'] += 1
                observed.append(guarded.read_bytes())
                if stage['call'] == 1:  # the bound positive control
                    return subprocess.CompletedProcess(command, 0, marker, '')
                return subprocess.CompletedProcess(command, 1, '', failure)

            with patch.object(runner, 'root', base), \
                    patch.object(runner, 'run_owned', fake_run_owned), \
                    patch.object(runner, 'write_source_atomic', recording_atomic):
                status, reason, compiled = runner.judge_row(row, original, out)
            message = ('#3110 the mutation publish and restore must each be one '
                       'atomic rename (write_source_atomic), never a truncating '
                       'write_text/write_bytes on the guarded source')
            self.assertEqual((status, compiled), ('KILLED', True),
                             f'the baseline drive itself is broken: {reason!r}')
            self.assertEqual(channel, [(guarded, mutated), (guarded, original)],
                             message + ' (publish then restore, exactly one '
                             'crossing each)')
            self.assertEqual(observed[0], original,
                             message + ' (the positive control reads pristine bytes)')
            self.assertEqual(observed[1], mutated,
                             message + ' (the mutant must be on disk before the '
                             'bound negative run reads it)')
            self.assertEqual(guarded.read_bytes(), original,
                             message + ' (the row must end restored)')


class EmptyTableControls(unittest.TestCase):
    """#3178 fix-r2 (P2-a): an emptied mutation table is a refusal, not a
    write-empty-summary-then-rc-1 finish.

    The tail's ``int(not results or ...)`` already refused to read an empty
    run as green (rc=1), but the publish before it still wrote an empty
    summary.json — a summary that exists reads like a completed run, and rc=1
    reads "ran and was red". The refusal must come before the evidence
    directory is claimed: exit 2, nothing on disk, re-runnable on the same
    output path.
    """

    def test_an_empty_mutation_table_refuses_before_any_evidence(self):
        import sys
        import tempfile
        runner = load_runner()

        def unreachable(*_args, **_kwargs):
            raise AssertionError('#3178 the empty table must refuse before any '
                                 'run, slot wait or source guard')

        with tempfile.TemporaryDirectory(prefix='pdrs-fm-empty-') as temp:
            out = Path(temp) / 'run'
            with patch.object(runner, 'mutations', []), \
                    patch.object(runner, 'mutating_sources', unreachable), \
                    patch.object(runner, 'run_owned', unreachable), \
                    patch.object(runner, 'wait_for_slot', unreachable), \
                    patch.object(sys, 'argv',
                                 ['negative_controls.py', str(out)]):
                code = runner.main()
            listing = sorted(path.name for path in out.iterdir()) \
                if out.exists() else None
        message = ('#3178 an empty mutation table must refuse before any '
                   'evidence (the old tail wrote an empty summary.json and '
                   'exited 1)')
        self.assertEqual(code, 2, message)
        self.assertIsNone(listing,
                          message + ' (nothing may be created for a run that '
                          'never started; the same output path must stay '
                          're-runnable)')


if __name__ == '__main__':
    unittest.main()
