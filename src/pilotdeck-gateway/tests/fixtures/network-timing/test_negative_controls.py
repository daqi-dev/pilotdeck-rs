#!/usr/bin/env python3
# anchor-selfcheck: unsupported meta-test of the network-timing startup source guards, spawns -O subprocesses only
"""#1922: an unreadable bound source is "never started" (2), not a failed run.

Two entries in this directory read product sources before any verdict:

* ``negative_controls.py`` re-reads ``network_timing.rs`` at module level to
  count the expected passes (#1922 item 3). The read sits before every
  ``refuse`` guard, so an unreadable file (EACCES / ENOENT) used to escape
  as an uncaught ``OSError`` with exit 1 -- the host triage table reads that
  as "ran and did not come out green", an investigation of a run that never
  began. It is now wrapped in ``except (OSError, ValueError) -> refuse()``:
  a non-UTF-8 source raises ``UnicodeDecodeError``, which is a ``ValueError``
  and would otherwise still land on exit 1.
* ``negative_private.py`` already wraps its ``catalog`` re-read in
  ``except (OSError, ValueError, RuntimeError) -> refuse()``, but no case
  ever exercised the ``OSError`` branch. Its ``--catalog`` early exit sat
  *outside* that block and is now covered too: the same unreadable source
  must refuse there as well (review r1 sibling P2).

Each refusal case execs the real script with exactly one injected
source-level edit -- the same shape an accidental path edit would take --
inside a ``python3 -O`` subprocess, because the whole point of these guards
is that they do not depend on ``assert``. ENOENT is injected by pointing at
a path that does not exist; EACCES by pointing at a mode-000 temporary file
(the scripts' repo-root join keeps an absolute path absolute); an undecodable
source by pointing at a file holding invalid UTF-8. Each must refuse: exit 2,
``INVALID`` on stderr, and (where the case receives an evidence path) no
evidence directory left behind. Two unedited positive controls prove the
harness reaches the real startup guards. Nothing here needs cargo, a committed
tree or a mutation.

Run from the fixtures root: ``python3 -m unittest network-timing/test_negative_controls.py``.
"""
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

HERE = Path(__file__).resolve().parent
FIXTURES = HERE.parent
CONTROLS_SCRIPT = HERE / 'negative_controls.py'
PRIVATE_SCRIPT = HERE / 'negative_private.py'
DRIVE_FLAG = '--drive-refusal'
UNKNOWN = 'no-such-mutation-1922'

# The module-level read whose OSError must become a refusal (#1922 item 3).
CONTROLS_READ = ("test_source = (root / "
                 "'src/pilotdeck-gateway/tests/network_timing.rs').read_text()")
# The first catalog source negative_private.py re-reads inside its refuse
# block; retargeting this one assignment is one whole edit.
PRIVATE_NETWORK = "network = 'src/pilotdeck-gateway/src/runtime/network.rs'"


def drive_refusal():
    """Run inside ``python3 -O``: exec one real script with one injected edit."""
    spec = json.loads(sys.argv[2])
    script = Path(spec['script'])
    source = script.read_text(encoding='utf-8')
    edit = spec.get('edit')
    if edit is not None:
        before, after = edit
        count = source.count(before)
        if count != 1:
            print(f'VERDICT BAD-ANCHOR {count}')
            return
        source = source.replace(before, after, 1)
    environ = {key: value for key, value in os.environ.items()
               if key != 'CARGO_TARGET_DIR'}
    if spec.get('target'):
        environ['CARGO_TARGET_DIR'] = spec['target']
    argv = [str(script), *spec.get('argv', [spec['out'], *spec.get('names', [])])]
    code = 0
    failure = ''
    try:
        with patch.dict(os.environ, environ, clear=True), \
                patch.object(sys, 'argv', argv):
            exec(compile(source, str(script), 'exec'),
                 {'__name__': '__main__', '__file__': str(script)})
    except SystemExit as error:
        code = error.code if isinstance(error.code, int) else 1
    except BaseException as error:
        # An uncaught OSError is exactly the pre-#1922 bug: rc 1 territory.
        code = 3
        failure = f'{type(error).__name__}: {error}'
    print(f'VERDICT {code} {failure}'.rstrip())


def eacces_is_stageable():
    """True when this host really refuses to read a mode-000 file.

    Review r3 boundary P2: ``geteuid() == 0`` is a proxy, not a measurement --
    a root container may still honour the mode bits, and an unprivileged host
    may not. Probe the actual behaviour instead of predicting it.
    """
    with tempfile.TemporaryDirectory(prefix='negctl-1922-probe-') as base:
        probe = Path(base) / 'mode-000'
        probe.write_text('probe\n', encoding='utf-8')
        os.chmod(probe, 0)
        try:
            probe.read_text()
        except PermissionError:
            return True
        return False


EACCES_STAGEABLE = eacces_is_stageable()


class UnreadableSourceRefusal(unittest.TestCase):
    """An unreadable bound source refuses (2) and leaves no evidence directory."""

    def drive(self, script, edit=None, *, names=(), target=False, argv=None):
        """Spawn ``python3 -O`` exec'ing ``script``.

        Returns ``(result, left_behind)``: whether the child created the
        evidence directory. It is sampled *inside* the ``with`` block -- after
        ``TemporaryDirectory.cleanup()`` the directory is always gone, so the
        same assertion taken later can never fail (review r1 concurrency /
        boundary P2).
        """
        with tempfile.TemporaryDirectory(prefix='negctl-1922-src-') as base:
            out = Path(base) / 'out'
            unreadable = Path(base) / 'unreadable-1922.rs'
            unreadable.write_text('static NEGCTL_1922: u8 = 0;\n', encoding='utf-8')
            os.chmod(unreadable, 0)
            # Not valid UTF-8: `read_text()` raises UnicodeDecodeError, a
            # ValueError rather than an OSError (review r2 boundary P2).
            undecodable = Path(base) / 'undecodable-1922.rs'
            undecodable.write_bytes(b'static NEGCTL_1922: u8 = 0;\n\xff\xfe\n')
            spec = dict(script=str(script), out=str(out), names=list(names),
                       edit=edit)
            if argv is not None:
                spec['argv'] = list(argv)
            if target:
                spec['target'] = str(Path(base) / 'cargo-target')
            if edit is not None:
                spec['edit'] = [edit[0], edit[1].format(unreadable=str(unreadable),
                                                        undecodable=str(undecodable))]
            result = subprocess.run(
                [sys.executable, '-O', str(Path(__file__).resolve()), DRIVE_FLAG,
                 json.dumps(spec)],
                cwd=str(FIXTURES), capture_output=True, text=True, timeout=180)
            left_behind = out.exists()
        return result, left_behind

    def verdict(self, result):
        lines = [line for line in result.stdout.split('\n') if line.strip()]
        self.assertTrue(lines != [], '#1922 the -O driver printed no verdict')
        return lines[-1]

    def assertRefused(self, result, left_behind, needle, *, claimed_path=True):
        last = self.verdict(result)
        self.assertTrue(last == 'VERDICT 2',
                        f'#1922 an unreadable bound source must refuse (rc=2), got {last!r}; '
                        f'stderr={result.stderr!r}')
        self.assertIn('INVALID', result.stderr,
                      '#1922 the refusal must be labelled INVALID on stderr')
        self.assertIn(needle, result.stderr,
                      '#1922 the refusal must name the unreadable source')
        if claimed_path:
            self.assertFalse(left_behind,
                             '#1922 a refusal before the run must not create the evidence directory')

    def test_controls_module_read_enoent_is_refused(self):
        # #1922 item 3: the module-level re-read of network_timing.rs.
        result, left_behind = self.drive(
            CONTROLS_SCRIPT,
            edit=(CONTROLS_READ,
                  "test_source = (root / "
                  "'src/pilotdeck-gateway/tests/no-such-file-1922.rs').read_text()"),
            names=[UNKNOWN])
        self.assertRefused(result, left_behind, 'No such file or directory')

    def test_controls_module_read_undecodable_is_refused(self):
        # Review r2 boundary P2: UnicodeDecodeError is a ValueError, so the
        # OSError-only guard left a corrupt source on exit 1.
        result, left_behind = self.drive(
            CONTROLS_SCRIPT,
            edit=(CONTROLS_READ,
                  "test_source = (root / '{undecodable}').read_text()"),
            names=[UNKNOWN])
        self.assertRefused(result, left_behind, "codec can't decode")

    @unittest.skipUnless(EACCES_STAGEABLE,
                         '#1922 this host can read a mode-000 file; EACCES cannot be staged')
    def test_controls_module_read_eacces_is_refused(self):
        result, left_behind = self.drive(
            CONTROLS_SCRIPT,
            edit=(CONTROLS_READ,
                  "test_source = (root / '{unreadable}').read_text()"),
            names=[UNKNOWN])
        self.assertRefused(result, left_behind, 'Permission denied')

    def test_private_catalog_enoent_is_refused(self):
        # negative_private.py's OSError -> refuse() branch, never exercised
        # before: the catalog re-read hits a missing source.
        result, left_behind = self.drive(
            PRIVATE_SCRIPT,
            edit=(PRIVATE_NETWORK,
                  "network = 'src/pilotdeck-gateway/src/runtime/no-such-1922.rs'"),
            names=[], target=True)
        self.assertRefused(result, left_behind, 'No such file or directory')

    def test_private_catalog_undecodable_is_refused(self):
        # Review r3 sibling P2: negative_private.py catches ValueError too, and
        # nothing exercised that half of the tuple at this entry.
        result, left_behind = self.drive(
            PRIVATE_SCRIPT,
            edit=(PRIVATE_NETWORK, "network = '{undecodable}'"),
            names=[], target=True)
        self.assertRefused(result, left_behind, "codec can't decode")

    @unittest.skipUnless(EACCES_STAGEABLE,
                         '#1922 this host can read a mode-000 file; EACCES cannot be staged')
    def test_private_catalog_eacces_is_refused(self):
        result, left_behind = self.drive(
            PRIVATE_SCRIPT,
            edit=(PRIVATE_NETWORK, "network = '{unreadable}'"),
            names=[], target=True)
        self.assertRefused(result, left_behind, 'Permission denied')

    def test_private_catalog_mode_enoent_is_refused(self):
        # #1922 review r1 sibling P2: the `--catalog` early exit also re-reads
        # every registered source, and it used to sit outside the startup
        # try/except -- an unreadable source there was an uncaught OSError
        # (exit 1, "ran but not green") instead of "never started" (2).
        result, left_behind = self.drive(
            PRIVATE_SCRIPT,
            edit=(PRIVATE_NETWORK,
                  "network = 'src/pilotdeck-gateway/src/runtime/no-such-1922.rs'"),
            argv=['--catalog'])
        # `--catalog` never receives `out`, so "no evidence directory" is not
        # observable here (review r2 simplify P2); what is observable is that a
        # refusal prints no partial catalog.
        self.assertRefused(result, left_behind, 'No such file or directory',
                           claimed_path=False)
        self.assertEqual(result.stdout.strip(), self.verdict(result),
                         '#1922 a refused catalog must not print a partial catalog')

    def test_private_catalog_mode_undecodable_is_refused(self):
        # Same ValueError half of the tuple, reached through `--catalog`.
        result, left_behind = self.drive(
            PRIVATE_SCRIPT,
            edit=(PRIVATE_NETWORK, "network = '{undecodable}'"),
            argv=['--catalog'])
        self.assertRefused(result, left_behind, "codec can't decode",
                           claimed_path=False)
        self.assertEqual(result.stdout.strip(), self.verdict(result),
                         '#1922 a refused catalog must not print a partial catalog')

    def test_controls_without_target_still_refuses_first_guard(self):
        # Positive control for the harness: unedited, the module-level read
        # succeeds and the run stops at the caller target guard.
        result, left_behind = self.drive(CONTROLS_SCRIPT, names=[UNKNOWN])
        last = self.verdict(result)
        self.assertTrue(last == 'VERDICT 2',
                        f'#1922 the unedited script must refuse, got {last!r}')
        self.assertIn('CARGO_TARGET_DIR must be supplied by the caller', result.stderr)
        self.assertFalse(left_behind)

    def test_private_unknown_name_still_refuses(self):
        # Positive control reaching the select/catalog startup block: the
        # real sources are all readable, so the unknown name refuses.
        result, left_behind = self.drive(PRIVATE_SCRIPT, names=[UNKNOWN], target=True)
        last = self.verdict(result)
        self.assertTrue(last == 'VERDICT 2',
                        f'#1922 the unedited script must refuse, got {last!r}')
        self.assertIn('unknown controls', result.stderr)
        self.assertIn(UNKNOWN, result.stderr)
        self.assertFalse(left_behind)


if __name__ == '__main__':
    if len(sys.argv) > 1 and sys.argv[1] == DRIVE_FLAG:
        drive_refusal()
    else:
        unittest.main()
