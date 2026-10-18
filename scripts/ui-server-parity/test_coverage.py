#!/usr/bin/env -S python3 -I
"""Offline gate controls. This is S2 coverage validation, not S3 ruler tests."""
# Isolated bootstrap: verify the source loader before executing local code.
import os as _boot_os
import sys as _boot_sys
if not (_boot_sys.flags.isolated and _boot_sys.flags.ignore_environment
        and _boot_sys.flags.no_user_site and _boot_sys.pycache_prefix is None):
    print('{"verdict":"FAIL","errors":["Python isolation required: use python3 -I; no pycache prefix"]}')
    raise SystemExit(1)
_boot_sys.dont_write_bytecode = True
_boot_dir = _boot_os.path.dirname(_boot_os.path.abspath(__file__))
_boot_source = _boot_os.path.join(_boot_dir, 'apparatus.py')
if _boot_os.path.islink(_boot_dir) or _boot_os.path.islink(_boot_source):
    print('{"verdict":"FAIL","errors":["apparatus bootstrap symlink"]}')
    raise SystemExit(1)
import hashlib as _boot_hashlib
import subprocess as _boot_subprocess
_boot_root = _boot_os.path.dirname(_boot_os.path.dirname(_boot_dir))
_boot_manifest = _boot_os.path.join(_boot_root, 'specs/ui-server-parity/FROZEN.md')
with open(_boot_source, 'rb') as _boot_file:
    _boot_bytes = _boot_file.read()
try:
    if _boot_os.path.isfile(_boot_manifest):
        with open(_boot_manifest) as _boot_file:
            _boot_pins = [line.split()[0] for line in _boot_file
                          if line.rstrip().endswith('  ../../scripts/ui-server-parity/apparatus.py')]
        if len(_boot_pins) != 1 or _boot_hashlib.sha256(_boot_bytes).hexdigest() != _boot_pins[0]:
            raise ValueError('apparatus bootstrap hash mismatch')
    else:
        _boot_trusted = _boot_subprocess.check_output(
            ['git', '-C', _boot_root, 'show', 'HEAD:scripts/ui-server-parity/apparatus.py'])
        if _boot_bytes != _boot_trusted:
            raise ValueError('apparatus bootstrap differs from HEAD without manifest')
except (OSError, ValueError, _boot_subprocess.CalledProcessError) as _boot_error:
    print(str(_boot_error), file=_boot_sys.stderr)
    print('{"verdict":"FAIL","errors":["apparatus bootstrap hash admission failed"]}')
    raise SystemExit(1)
_boot_namespace = {'__file__': _boot_source, '__name__': '_apparatus_bootstrap'}
exec(compile(_boot_bytes, _boot_source, 'exec'), _boot_namespace)
_boot_namespace['preflight'](_boot_dir)
import copy
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from check_coverage import check, rust_routes
from unittest.mock import patch

HERE = Path(__file__).resolve().parent
ENUM = {'appCaptured': True, 'uniqueRouteCount': 2, 'routes': [
    {'method': 'GET', 'path': '/api/a'}, {'method': 'POST', 'path': '/api/b/:id'}]}
SCENARIOS = {'http_cases': [{'id': 'a', 'transport': 'http', 'route': 'GET /api/a', 'expected_statuses': [200]}]}
MATRIX = {'routes': [{'route': 'GET /api/a', 'cases': ['a']},
    {'route': 'POST /api/b/:id', 'cases': [], 'exemption': {'route': 'POST /api/b/:id',
        'reason': 'requires isolated write fixture', 'approver': 'AI 代批，待人复核'}}]}


def successful_evidence():
    return [{'kind': 'case_http', 'case': 'a', 'route': 'GET /api/a',
             'side': side, 'ok': True, 'status': 200, 'method': 'GET', 'path': '/api/a',
             'handler': 'ts' if side == 'proxy' else None} for side in ('direct', 'proxy')]


class CoverageControls(unittest.TestCase):
    def test_missing_route_cli_fails_then_complete_passes(self):
        with tempfile.TemporaryDirectory(prefix='exp010-coverage-') as directory:
            root = Path(directory)
            for name, value in [('enum', ENUM), ('matrix', {'routes': MATRIX['routes'][:1]}), ('scenarios', SCENARIOS)]:
                (root / f'{name}.json').write_text(json.dumps(value))
            command = [sys.executable, '-I', str(HERE / 'check_coverage.py'), '--enumeration', str(root / 'enum.json'),
                       '--matrix', str(root / 'matrix.json'), '--scenarios', str(root / 'scenarios.json')]
            missing = subprocess.run(command, capture_output=True, text=True)
            self.assertEqual(missing.returncode, 1)
            self.assertIn('POST /api/b/:id: absent from matrix', missing.stdout)
            (root / 'matrix.json').write_text(json.dumps(MATRIX))
            fixed = subprocess.run(command, capture_output=True, text=True)
            self.assertEqual(fixed.returncode, 0)
            self.assertEqual(json.loads(fixed.stdout)['verdict'], 'PASS')
            print('CONTROL missing-route exit=1/FAIL; complete exit=0/PASS')

    def test_malformed_waiver_and_ws_case_rejected(self):
        matrix = copy.deepcopy(MATRIX)
        del matrix['routes'][1]['exemption']['approver']
        self.assertEqual(check(ENUM, matrix, SCENARIOS)['verdict'], 'FAIL')
        scenarios = copy.deepcopy(SCENARIOS)
        scenarios['http_cases'][0]['transport'] = 'ws'
        self.assertEqual(check(ENUM, MATRIX, scenarios)['verdict'], 'FAIL')

    def test_declared_case_cannot_substitute_for_evidence(self):
        evidence = successful_evidence()
        self.assertEqual(check(ENUM, MATRIX, SCENARIOS, evidence)['verdict'], 'PASS')
        self.assertEqual(check(ENUM, MATRIX, SCENARIOS, evidence[:1])['verdict'], 'FAIL')
        evidence[1]['method'] = 'POST'
        self.assertEqual(check(ENUM, MATRIX, SCENARIOS, evidence)['verdict'], 'FAIL')

    def test_static_rust_waiver_requires_human_approval_without_cases(self):
        takeover = {'schema': 1, 'routes': [{'method': 'POST', 'path': '/api/b/:id', 'handler': 'rust'}]}
        self.assertEqual(check(ENUM, MATRIX, SCENARIOS, takeover=takeover)['verdict'], 'FAIL')
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            allowlist = root / 'specs/ui-server-parity/human-approvers.txt'
            allowlist.parent.mkdir(parents=True)
            # Both the configured identity and Approved-By use the same normalization.
            allowlist.write_text('ＺＨＥＮＧＤＡＱＩ\u200b\n', encoding='utf-8')
            basis = []
            for name in ('report.json', 'review-one.md', 'review-two.md'):
                path = root / name
                path.write_text('evidence')
                basis.append(str(path))
            matrix = copy.deepcopy(MATRIX)
            waiver = matrix['routes'][1]['exemption']
            waiver['Approval-Basis'] = '; '.join(basis)
            with patch('check_coverage.ROOT', root):
                for approver in ('zhengdaqi', 'ZHENGDAQI', 'ｚｈｅｎｇｄａｑｉ',
                                 ' zheng\t da\nqi\u00a0', 'zheng\u200bdaqi',
                                 'zheng\ufeffdaqi', 'zheng\u034fdaqi', 'zheng\u3164daqi'):
                    waiver['Approved-By'] = approver
                    # Zero-width variants resolving to an allowlisted human intentionally PASS.
                    with self.subTest(approver=approver):
                        self.assertEqual(check(ENUM, matrix, SCENARIOS, takeover=takeover)['verdict'], 'PASS')
                rejected = ('AI 代批，待人复核（协调者）', 'AI', 'AI 代批，待人复核',
                            '代批：张三', '协调者', '张三（协调者）', '待批准',
                            'Claude', 'GPT批准', 'GLM', 'DeepSeek', 'Gemini', 'Qwen',
                            'Doubao', 'ChatGPT', '模型', '张三', '李四',
                            'ＡＩ批准', 'Ｃｌａｕｄｅ', '协 调者', 'Opus 5.5', '人工智能',
                            '', ' \t\n\u200b', None, 123, 'zhengdaqi-approved',
                            'approved-zhengdaqi', 'zhengdaqi（协调者）')
                # Invisible characters must not disguise any forbidden identity.
                for approver in rejected + tuple(value[:1] + '\u200b' + value[1:]
                                                  for value in rejected if isinstance(value, str)):
                    waiver['Approved-By'] = approver
                    with self.subTest(approver=approver):
                        self.assertEqual(check(ENUM, matrix, SCENARIOS, takeover=takeover)['verdict'], 'FAIL')
                waiver['Approved-By'] = 'zhengdaqi'
                for bad_basis in ('; '.join(basis[:2]), '; '.join([basis[0], basis[1], basis[1]]),
                                  '; '.join([basis[0], basis[1], str(root / 'missing.md')])):
                    waiver['Approval-Basis'] = bad_basis
                    with self.subTest(basis=bad_basis):
                        self.assertEqual(check(ENUM, matrix, SCENARIOS, takeover=takeover)['verdict'], 'FAIL')
                outside = root.parent / (root.name + '-outside.md')
                outside.write_text('outside evidence')
                try:
                    waiver['Approval-Basis'] = '; '.join([basis[0], basis[1], str(outside)])
                    self.assertEqual(check(ENUM, matrix, SCENARIOS, takeover=takeover)['verdict'], 'FAIL')
                finally:
                    outside.unlink()
                del waiver['Approval-Basis']
                self.assertEqual(check(ENUM, matrix, SCENARIOS, takeover=takeover)['verdict'], 'FAIL')
                waiver['Approval-Basis'] = str(root / 'missing.md')
                self.assertEqual(check(ENUM, matrix, SCENARIOS, takeover=takeover)['verdict'], 'FAIL')
                waiver['Approval-Basis'] = '; '.join(basis)
                for contents in ('', '\u200b\n', 'someone-else\n'):
                    allowlist.write_text(contents, encoding='utf-8')
                    with self.subTest(allowlist=contents):
                        self.assertEqual(check(ENUM, matrix, SCENARIOS, takeover=takeover)['verdict'], 'FAIL')
                allowlist.unlink()
                self.assertEqual(check(ENUM, matrix, SCENARIOS, takeover=takeover)['verdict'], 'FAIL')

    def test_allowlist_comments_blanks_and_padding(self):
        from apparatus import human_approvers
        commented = '# owner list\n\n  zhengdaqi  \n\t\n ​# indented\n＃ full-width\n#\n'
        self.assertEqual(human_approvers(commented), {'zhengdaqi'})
        self.assertEqual(human_approvers('# only comments\n\n'), set())
        # An inline '#' leaves the identity ambiguous: the whole list fails closed.
        self.assertEqual(human_approvers('zhengdaqi\nalice # contractor\n'), set())
        takeover = {'schema': 1, 'routes': [{'method': 'POST', 'path': '/api/b/:id', 'handler': 'rust'}]}
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            allowlist = root / 'specs/ui-server-parity/human-approvers.txt'
            allowlist.parent.mkdir(parents=True)
            allowlist.write_text(commented, encoding='utf-8')
            basis = []
            for name in ('report.json', 'review-one.md', 'review-two.md'):
                (root / name).write_text('evidence')
                basis.append(str(root / name))
            matrix = copy.deepcopy(MATRIX)
            waiver = matrix['routes'][1]['exemption']
            waiver['Approval-Basis'] = '; '.join(basis)
            with patch('check_coverage.ROOT', root):
                for approver, verdict in (('zhengdaqi', 'PASS'), (' zhengdaqi\t', 'PASS'),
                                          ('#ownerlist', 'FAIL'), ('# owner list', 'FAIL'),
                                          ('#', 'FAIL'), ('＃full-width', 'FAIL'), ('#indented', 'FAIL')):
                    waiver['Approved-By'] = approver
                    with self.subTest(approver=approver):
                        self.assertEqual(check(ENUM, matrix, SCENARIOS, takeover=takeover)['verdict'], verdict)
                allowlist.write_text('zhengdaqi\nalice # contractor\n', encoding='utf-8')
                waiver['Approved-By'] = 'zhengdaqi'
                self.assertEqual(check(ENUM, matrix, SCENARIOS, takeover=takeover)['verdict'], 'FAIL')

    def test_invalid_and_unlisted_handlers_cannot_hide_takeover(self):
        for handler in ('Rust', 'rust-v2', 'AI', 'rust'):
            evidence = successful_evidence() + [{'kind': 'case_http', 'side': 'proxy',
                'route': 'POST /api/b/:id', 'handler': handler, 'ok': False}]
            self.assertEqual(check(ENUM, MATRIX, SCENARIOS, evidence)['verdict'], 'FAIL')
        self.assertEqual(rust_routes({'schema': 1, 'routes': []}), set())
        for manifest in ({'schema': True, 'routes': []}, {'schema': 1, 'routes': [
                {'method': 'POST', 'path': '/api/b', 'handler': 'Rust'}]}):
            with self.assertRaises(ValueError):
                rust_routes(manifest)

    def test_non_rust_ai_waivers_still_pass(self):
        for handler in ('ts', None):
            with self.subTest(handler=handler):
                evidence = successful_evidence()
                evidence.append({'kind': 'case_http', 'route': 'POST /api/b/:id', 'handler': handler})
                self.assertEqual(check(ENUM, MATRIX, SCENARIOS, evidence)['verdict'], 'PASS')

    def test_normalization_collision_fails(self):
        enumeration = copy.deepcopy(ENUM)
        enumeration['routes'].append({'method': 'GET', 'path': '/api/a/'})
        enumeration['uniqueRouteCount'] = 3
        self.assertEqual(check(enumeration, MATRIX, SCENARIOS)['verdict'], 'FAIL')

    def test_empty_enumeration_and_stale_matrix_fail(self):
        self.assertEqual(check({'appCaptured': True, 'uniqueRouteCount': 0, 'routes': []}, MATRIX, SCENARIOS)['verdict'], 'FAIL')


if __name__ == '__main__':
    unittest.main()
