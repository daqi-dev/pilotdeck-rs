#!/usr/bin/env python3
"""Projection negative controls: semantic IDs and non-registry time must survive."""
import copy
import importlib.util
import unittest
from pathlib import Path
spec = importlib.util.spec_from_file_location('summary', Path(__file__).with_name('summarize-ws-b.py'))
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)

class Projection(unittest.TestCase):
    def setUp(self):
        self.o = dict(connections=[dict(name='A', binding=dict(connectionId='uuid', generation=8), frames=[dict(type='event', id='request', event=dict(runId='run', createdAt='fixed', connectionId='uuid', generation=8))])],
                      durable=[], registry=dict(records=[dict(runId='run', acceptedAt='wall', updatedAt='wall', ownerPid=1)], events={}), timeline=[])
    def test_binding_bijection(self):
        other = copy.deepcopy(self.o)
        other['connections'][0]['binding'] = dict(connectionId='another', generation=10)
        other['connections'][0]['frames'][0]['event'].update(connectionId='another', generation=10)
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

if __name__ == '__main__':
    unittest.main()
