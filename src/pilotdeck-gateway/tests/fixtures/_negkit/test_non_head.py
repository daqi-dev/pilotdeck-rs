#!/usr/bin/env python3
# anchor-selfcheck: unsupported #1540 non-HEAD verdict unit tests; disposable files only
"""Isolated tests of the shared non-HEAD-baseline verdict (#1540).

Every check pins one clause of the exit-42 contract: the HEAD path stays
byte-identical (silent, no extra subprocess, exit 0), a non-HEAD baseline is
announced, a green non-HEAD run is downgraded to 42 with the stdout marker and
the ``baseline.json`` provenance record, and any non-green verdict keeps its
own code.  The paired mutation control is ``negative_non_head.py``.
"""
from contextlib import redirect_stderr, redirect_stdout
import io
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parent))
import non_head


class GateTests(unittest.TestCase):
    longMessage = True

    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix='negkit-non-head-test-')
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name).resolve()
        self.sha = 'a' * 40

    def resolve_ok(self, command, **_kwargs):
        self.assertTrue(command[:4] == ['git', '-C', str(self.root), 'rev-parse']
                        and command[4:6] == ['--verify', 'stash-ref^{commit}'],
                        '#1540 the override is resolved inside the repo, once')
        return subprocess.CompletedProcess(command, 0, self.sha + '\n', '')

    def test_unset_baseline_is_head_and_silent(self):
        with patch.object(non_head.subprocess, 'run') as git:
            with redirect_stdout(io.StringIO()) as stdout:
                triple = non_head.gate(self.root, {})
        self.assertTrue(triple == ('HEAD', 'HEAD', None),
                        '#1540 an unset override keeps the silent HEAD path')
        self.assertTrue(stdout.getvalue() == '',
                        '#1540 a HEAD run must not print a baseline line')
        git.assert_not_called()

    def test_empty_override_means_head(self):
        resolved = subprocess.CompletedProcess([], 0, self.sha + '\n', '')
        with patch.object(non_head.subprocess, 'run', return_value=resolved) as git:
            with redirect_stdout(io.StringIO()) as stdout:
                triple = non_head.gate(self.root, {'PDRS_NEGATIVE_BASELINE': ''})
        self.assertTrue(triple == ('HEAD', 'HEAD', None),
                        '#1540 an empty override is unset, not a revision')
        self.assertTrue(stdout.getvalue() == '',
                        '#1540 an empty override must not announce anything')
        git.assert_not_called()

    def test_override_resolves_and_announces(self):
        with patch.object(non_head.subprocess, 'run', self.resolve_ok):
            with redirect_stdout(io.StringIO()) as stdout:
                triple = non_head.gate(self.root, {'PDRS_NEGATIVE_BASELINE': 'stash-ref'})
        self.assertTrue(triple == ('PDRS_NEGATIVE_BASELINE', 'stash-ref', self.sha),
                        '#1540 the pinned override travels on with its resolved SHA')
        self.assertTrue(f'NON-HEAD BASELINE: {self.sha} via PDRS_NEGATIVE_BASELINE'
                        in stdout.getvalue(),
                        '#1540 a non-HEAD run announces itself before any mutant runs')

    def test_ignored_override_announces_the_ignore(self):
        with patch.object(non_head.subprocess, 'run', self.resolve_ok):
            with redirect_stdout(io.StringIO()) as stdout:
                triple = non_head.gate(self.root, {'PDRS_NEGATIVE_BASELINE': 'stash-ref'},
                                       ignored=True)
        self.assertTrue(triple[0] == 'PDRS_NEGATIVE_BASELINE',
                        '#1540 an ignored override is still a non-HEAD request')
        self.assertTrue('ignores the override and always guards HEAD' in stdout.getvalue(),
                        '#1540 the ignored flavor must say the guard stayed on HEAD')

    def test_unresolvable_override_refuses(self):
        failure = subprocess.CompletedProcess([], 128, '', 'fatal: bad revision')
        with patch.object(non_head.subprocess, 'run', return_value=failure):
            with redirect_stderr(io.StringIO()) as stderr:
                try:
                    non_head.gate(self.root, {'PDRS_NEGATIVE_BASELINE': 'not-a-commit'})
                except SystemExit as raised:
                    code = raised.code
                else:
                    code = None
        self.assertTrue(code == 2,
                        '#1540 a garbage override never starts')
        self.assertTrue('cannot resolve PDRS_NEGATIVE_BASELINE' in stderr.getvalue(),
                        '#1540 the refusal names the variable')

    def test_resolver_crash_refuses(self):
        def crash(command, **_kwargs):
            raise subprocess.TimeoutExpired(command, 60)
        with patch.object(non_head.subprocess, 'run', side_effect=crash):
            with redirect_stderr(io.StringIO()) as stderr:
                try:
                    non_head.gate(self.root, {'PDRS_NEGATIVE_BASELINE': 'stash-ref'})
                except SystemExit as raised:
                    code = raised.code
                else:
                    code = None
        self.assertTrue(code == 2,
                        '#1540 a resolver crash is a refusal, never a traceback exit 1')
        self.assertTrue('cannot resolve PDRS_NEGATIVE_BASELINE' in stderr.getvalue(),
                        '#1540 the crash refusal names the variable too')


class VerdictTests(unittest.TestCase):
    longMessage = True

    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix='negkit-non-head-verdict-')
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name).resolve()
        self.out = self.root / 'evidence'
        self.out.mkdir()
        self.sha = 'b' * 40

    def test_failure_keeps_its_code(self):
        for code in (1, 2):
            with redirect_stdout(io.StringIO()) as stdout:
                result = non_head.non_head_exit('PDRS_NEGATIVE_BASELINE', self.sha, code=code)
            self.assertTrue(result == code,
                            '#1540 a failing run keeps its own exit code')
            self.assertTrue(stdout.getvalue() == '',
                            '#1540 a failing run gets no non-host marker')

    def test_head_green_exits_zero_without_side_effects(self):
        with redirect_stdout(io.StringIO()) as stdout:
            result = non_head.non_head_exit('HEAD', None, evidence=self.out)
        self.assertTrue(result == 0, '#1540 a HEAD green run still exits 0')
        self.assertTrue(stdout.getvalue() == '', '#1540 a HEAD green run prints nothing')
        self.assertTrue(not (self.out / 'baseline.json').exists(),
                        '#1540 a HEAD green run writes no provenance override')

    def test_non_head_green_marks_and_exits_42(self):
        published = []
        raw_replace = Path.replace

        def replace(path, target):
            published.append(path.name)
            return raw_replace(path, target)

        with patch.object(Path, 'replace', replace):
            with redirect_stdout(io.StringIO()) as stdout:
                result = non_head.non_head_exit('PDRS_NEGATIVE_BASELINE', self.sha,
                                                evidence=self.out)
        self.assertTrue(result == non_head.NON_HEAD_EXIT == 42,
                        '#1540 a green non-HEAD run exits the dedicated code 42')
        self.assertTrue(f'NON-HOST EVIDENCE (非宿主证据): baseline {self.sha} '
                        'came from PDRS_NEGATIVE_BASELINE, not HEAD' in stdout.getvalue()
                        and 'exit 42' in stdout.getvalue(),
                        '#1540 the stdout marker names the baseline and the code')
        self.assertTrue((self.out / 'baseline.json').is_file(),
                        '#1540 the provenance record is published for the host gate')
        record = json.loads((self.out / 'baseline.json').read_text(encoding='utf-8'))
        self.assertTrue(record == {'baseline': self.sha,
                                  'baselineSource': 'PDRS_NEGATIVE_BASELINE',
                                  'nonHeadBaseline': True, 'hostEvidence': False,
                                  'exit': 42},
                        '#1540 baseline.json records the non-host provenance')
        self.assertTrue(published == ['baseline.json.tmp'],
                        '#1540 the provenance record is published through a temp file')
        self.assertTrue(not (self.out / 'baseline.json.tmp').exists(),
                        '#1540 the provenance record is published, not left as a temp')

    def test_ignored_message_names_the_ignore(self):
        with redirect_stdout(io.StringIO()) as stdout:
            result = non_head.non_head_exit('PDRS_NEGATIVE_BASELINE', self.sha,
                                            evidence=self.out, ignored=True)
        self.assertTrue(result == 42, '#1540 the ignored flavor still exits 42')
        self.assertTrue('ignores (its guard always requires HEAD)' in stdout.getvalue()
                        and 'cannot certify the named baseline' in stdout.getvalue(),
                        '#1540 the ignored marker says the run cannot certify the request')
        self.assertTrue((self.out / 'baseline.json').exists(),
                        '#1540 the ignored flavor still records provenance')

    def test_abnormal_verdict_normalizes_to_one(self):
        for code in (None, 256, True):
            with redirect_stdout(io.StringIO()) as stdout:
                result = non_head.non_head_exit('PDRS_NEGATIVE_BASELINE', self.sha,
                                                code=code)
            self.assertTrue(result == 1,
                            '#1540 an unusable verdict must not become a green exit')
            self.assertTrue(stdout.getvalue() == '',
                            '#1540 an unusable verdict gets no non-host marker')

    def test_ignored_record_is_honest_about_head(self):
        with redirect_stdout(io.StringIO()) as stdout:
            result = non_head.non_head_exit('PDRS_NEGATIVE_BASELINE', self.sha,
                                            evidence=self.out, ignored=True)
        self.assertTrue(result == 42, '#1540 the ignored flavor still exits 42')
        record = json.loads((self.out / 'baseline.json').read_text(encoding='utf-8'))
        self.assertTrue(record['baseline'] == 'HEAD'
                        and record['requestedBaseline'] == self.sha
                        and record['overrideIgnored'] is True
                        and record['hostEvidence'] is False,
                        '#1540 the ignored record says HEAD was tested, not the request')

    def test_without_evidence_directory_nothing_is_written(self):
        with redirect_stdout(io.StringIO()) as stdout:
            result = non_head.non_head_exit('PDRS_NEGATIVE_BASELINE', self.sha)
        self.assertTrue(result == 42, '#1540 no evidence directory still exits 42')
        self.assertTrue('NON-HOST EVIDENCE' in stdout.getvalue(),
                        '#1540 no evidence directory still marks stdout')
        self.assertTrue(not (self.root / 'baseline.json').exists(),
                        '#1540 the marker alone must not create files')


if __name__ == '__main__':
    unittest.main(verbosity=2)
