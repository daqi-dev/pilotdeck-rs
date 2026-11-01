#!/usr/bin/env python3
# anchor-selfcheck: unsupported parametrized read-only entry regressions, no source mutations
"""#1066 real-entry startup regressions; run by filename with python3 -I."""
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[5]
PROBE = Path(__file__).with_name('probe.py')
ENTRIES = (
    'scripts/gateway-parity/run_round.py',
    'scripts/gateway-parity/compare.py',
    'scripts/gateway-parity/check_frozen.py',
    'scripts/gateway-parity/check_ruler.py',
    'scripts/gateway-parity/build_proxy.py',
    'scripts/gateway-parity/observer.py',
    'scripts/gateway-parity/native_evidence.py',
    'scripts/gateway-parity/session_domain.py',
    'scripts/gateway-parity/test_safety.py',
    'scripts/gateway-parity/controls/run_all.py',
    'scripts/gateway-parity/controls/prerequisites.py',
    'scripts/gateway-parity/controls/session_domain.py',
    'scripts/parity/score.py',
    'scripts/parity/check_frozen.py',
    'scripts/parity/check_ruler.py',
    'scripts/parity/g03_compare.py',
    'scripts/parity/round.py',
    'scripts/parity/self_compare.py',
    'scripts/parity/controls/lock_release.py',
    'scripts/parity/controls/run_all.py',
    'scripts/parity/controls/multimodal.py',
    'scripts/parity/controls/review_r1.py',
    'scripts/check-approval-provenance.py',
    'scripts/test-approval-provenance.py',
)
REINSERT = 'apparatus directory on sys.path; run the entry file directly with -I'
PREFLIGHT = 'os must be preloaded from CPython 3.11+ frozen modules'


def test_name(relative, kind):
    return 'test_' + relative.removeprefix('scripts/').replace('/', '_').replace('-', '_').replace('.', '_') + '_' + kind


class EntryGuardTests(unittest.TestCase):
    def probe(self, relative, inserted='<absent>', premise='normal', *, cwd=ROOT, isolated=True):
        try:
            return subprocess.run(
                [sys.executable, *(['-I'] if isolated else []), '-B', str(PROBE),
                 str(ROOT / relative), inserted, premise],
                cwd=cwd, capture_output=True, text=True, timeout=15)
        except (OSError, subprocess.TimeoutExpired) as error:
            self.fail(f'#1066 {relative}: probe could not run: {error}')

    def reinsert(self, relative):
        folder = (ROOT / relative).parent
        if folder.name == 'controls':
            folder = folder.parent
        roots = [folder]
        if folder.name in ('gateway-parity', 'parity'):
            roots.append(folder / 'controls')
        with tempfile.TemporaryDirectory(prefix='pdrs-1066-') as temp:
            for index, target in enumerate(roots):
                alias = Path(temp) / str(index)
                try:
                    alias.symlink_to(target, target_is_directory=True)
                except OSError as error:
                    self.fail(f'#1066 {relative}: cannot create identity alias: {error}')
                spellings = [str(target), str(alias) + '//.', '.', '']
                alternate = str(target).replace('scripts', 'SCRIPTS', 1)
                if os.path.isdir(alternate) and os.path.samefile(alternate, target):
                    spellings.append(alternate)
                for spelling in spellings:
                    with self.subTest(directory=str(target), spelling=spelling):
                        result = self.probe(relative, spelling, cwd=target)
                        self.assertTrue(
                            (result.returncode, result.stdout, result.stderr.strip()) ==
                            (1, '', REINSERT),
                            f'#1066 {relative}: reinsertion must stop before stdlib')

    def preflight(self, relative, premise):
        result = self.probe(relative, premise=premise)
        self.assertTrue(
            (result.returncode, result.stdout, result.stderr.strip()) ==
            (1, '', PREFLIGHT),
            f'#1066 {relative}: {premise} must stop before os import')

    def clean(self, relative):
        with tempfile.TemporaryDirectory(prefix='pdrs-1066-clean-') as temp:
            for inserted in ('<absent>', str(Path(temp) / 'missing'), str(PROBE), temp):
                with self.subTest(inserted=inserted):
                    result = self.probe(relative, inserted)
                    self.assertEqual(result.returncode, 73,
                                     f'#1066 {relative}: safe paths must reach stdlib: {result.stderr}')
                    self.assertTrue(result.stderr.startswith('#1066 reached first stdlib import: '),
                                    f'#1066 {relative}: missing import boundary witness')
        result = self.probe(relative, isolated=False)
        self.assertEqual(result.returncode, 1,
                         f'#1066 {relative}: non-isolated entry must be rejected')
        self.assertIn('python3 -I', result.stderr,
                      f'#1066 {relative}: non-isolated entry must report -I')
        self.assertNotIn('#1066 reached', result.stderr,
                         f'#1066 {relative}: non-isolated entry imported stdlib')


def regression(relative, kind):
    def run(self):
        if kind in ('old-python', 'unloaded-os'):
            self.preflight(relative, kind)
        else:
            getattr(self, kind)(relative)
    return run


for relative in ENTRIES:
    for kind in ('reinsert', 'old-python', 'unloaded-os', 'clean'):
        setattr(EntryGuardTests, test_name(relative, kind).replace('-', '_'), regression(relative, kind))


if __name__ == '__main__':
    unittest.main(verbosity=2)
