#!/usr/bin/env python3
"""Offline gate controls. This is S2 coverage validation, not S3 ruler tests."""
import copy
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from check_coverage import check

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
            command = [sys.executable, str(HERE / 'check_coverage.py'), '--enumeration', str(root / 'enum.json'),
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

    def test_rust_ai_waiver_fails_and_human_approval_passes(self):
        evidence = successful_evidence()
        # This exempt route has no declared case. Even unsuccessful Rust
        # evidence must trigger the approval gate rather than be filtered out.
        evidence.append({'kind': 'case_http', 'case': 'b.probe', 'route': 'POST /api/b/:id',
                         'side': 'proxy', 'ok': False, 'status': 500,
                         'method': 'POST', 'path': '/api/b/42', 'handler': 'rust'})
        failed = check(ENUM, MATRIX, SCENARIOS, evidence)
        self.assertEqual(failed['verdict'], 'FAIL')
        self.assertTrue(any('human approval' in error for error in failed['errors']))
        matrix = copy.deepcopy(MATRIX)
        matrix['routes'][1]['exemption']['approver'] = '协调者人工批准'
        self.assertEqual(check(ENUM, matrix, SCENARIOS, evidence)['verdict'], 'PASS')
        # Declaration alone is not execution evidence and retains its scope.
        self.assertEqual(check(ENUM, MATRIX, SCENARIOS)['verdict'], 'PASS')
        print('CONTROL Rust + AI waiver FAIL; Rust + human waiver PASS')

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
