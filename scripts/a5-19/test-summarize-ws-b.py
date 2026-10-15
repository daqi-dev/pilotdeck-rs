#!/usr/bin/env python3
"""Projection negative controls: semantic IDs and non-registry time must survive."""
import copy
import importlib.util
import json
import hashlib
import tempfile
from datetime import date
import unittest
from pathlib import Path
spec = importlib.util.spec_from_file_location('summary', Path(__file__).with_name('summarize-ws-b.py'))
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)

class Projection(unittest.TestCase):
    def setUp(self):
        self.o = dict(connections=[dict(name='A', binding=dict(connectionId='uuid', generation=8), frames=[dict(type='event', id='request', event=dict(runId='run', createdAt='fixed', connectionId='uuid', generation=8))])],
                      durable=[], registry=dict(records=[dict(runId='run', acceptedAt='wall', updatedAt='wall', ownerPid=1)], events={}), timeline=[])
        self.o['connections'].append(dict(name='B', binding=dict(connectionId='peer', generation=9), frames=[]))
    def test_binding_bijection(self):
        other = copy.deepcopy(self.o)
        other['connections'][0]['binding'] = dict(connectionId='another', generation=10)
        other['connections'][0]['frames'][0]['event'].update(connectionId='another', generation=10)
        other['connections'][1]['binding']['generation'] = 11
        self.assertEqual(module.project(self.o), module.project(other))
    def test_semantic_id_retained(self):
        other = copy.deepcopy(self.o)
        other['connections'][0]['frames'][0]['event']['runId'] = 'wrong'
        self.assertNotEqual(module.project(self.o), module.project(other))
    def test_non_registry_time_retained(self):
        other = copy.deepcopy(self.o)
        other['connections'][0]['frames'][0]['event']['createdAt'] = 'wrong'
        self.assertNotEqual(module.project(self.o), module.project(other))
    def test_nested_registry_time_retained(self):
        other = copy.deepcopy(self.o)
        self.o['registry']['records'][0]['result'] = dict(acceptedAt='semantic')
        other['registry']['records'][0]['result'] = dict(acceptedAt='wrong')
        self.assertNotEqual(module.project(self.o), module.project(other))
    def test_terminal_retained(self):
        other = copy.deepcopy(self.o)
        other['registry']['records'][0]['result'] = dict(type='error', code='wrong')
        self.assertNotEqual(module.project(self.o), module.project(other))
    def test_actual_connections_and_binding_identity_required(self):
        other = copy.deepcopy(self.o)
        other['connections'].pop()
        with self.assertRaises(AssertionError): module.project(other)
        other = copy.deepcopy(self.o)
        other['connections'][1]['binding']['connectionId'] = 'uuid'
        with self.assertRaises(AssertionError): module.project(other)
    def test_report_differences_and_instability_reject(self):
        path = Path(__file__).resolve().parents[2] / 'docs/experiments/exp015-a52b-session-fixtures/ws-results.json'
        valid = json.loads(path.read_text())
        module.require_match(valid)
        bad = copy.deepcopy(valid)
        bad['cases'][0]['mainComparisons'][0]['differences'] = ['/durable/0/entries/0']
        with self.assertRaises(AssertionError): module.require_match(bad)
        bad = copy.deepcopy(valid)
        bad['cases'][0]['distinct'] = 2
        with self.assertRaises(AssertionError): module.require_match(bad)
    def test_historical_bytes_must_match_index(self):
        parent = Path(__file__).resolve().parents[2] / '_tmp' / (date.today().strftime('%Y%m%d') + '-exp015-controls')
        parent.mkdir(parents=True, exist_ok=True)
        with tempfile.TemporaryDirectory(dir=parent) as name:
            path = Path(name) / 'observation.json'
            raw = b'{"valid":true}'
            path.write_bytes(raw)
            digest = hashlib.sha256(raw).hexdigest()
            self.assertEqual(module.read_verified(path, digest), {'valid': True})
            path.write_bytes(b'{"valid":false}')
            with self.assertRaises(AssertionError): module.read_verified(path, digest)

if __name__ == '__main__':
    unittest.main()
