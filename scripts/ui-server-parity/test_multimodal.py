"""Admission gates; fixtures are fabricated metadata, never calibration evidence."""
# Isolated bootstrap: verify the source loader before executing local code.
import sys as _boot_sys
if not (_boot_sys.flags.isolated and _boot_sys.flags.ignore_environment
        and _boot_sys.flags.no_user_site and _boot_sys.pycache_prefix is None):
    print('{"verdict":"FAIL","errors":["Python isolation required: use python3 -I; no pycache prefix"]}')
    raise SystemExit(1)
if _boot_sys.version_info < (3, 11) or 'os' not in _boot_sys.modules:
    print('{"verdict":"FAIL","errors":["os must be preloaded from CPython 3.11+ frozen modules"]}')
    raise SystemExit(1)
import os as _boot_os
_boot_sys.dont_write_bytecode = True
_boot_dir = _boot_os.path.dirname(_boot_os.path.abspath(__file__))
# Identity, not spelling: a case-insensitive alias, a symlinked spelling or a
# mixed ``//`` form names this very directory while the string differs
# (fix-r3 P1, same defect as #701 fix-r2). Nonexistent sys.path entries skip.
if any(
        _boot_os.path.isdir(entry or _boot_os.curdir)
        and any(_boot_os.path.isdir(root)
                and _boot_os.path.samefile(entry or _boot_os.curdir, root)
                for root in (_boot_dir, _boot_os.path.join(_boot_dir, 'controls')))
        for entry in _boot_sys.path):
    print('{"verdict":"FAIL","errors":["apparatus directory on sys.path; run the entry file directly with -I"]}')
    raise SystemExit(1)
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
import tempfile
import unittest
from unittest.mock import patch

from common import apparatus_hashes
from controls.run_all import clock_evidence
from multimodal import (capture_identity, ReferencePool, confirmed_classes, compare_with_pool,
                        pool_replay_provenance, registered_fields)
from types import SimpleNamespace


class AdmissionBoundaries(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory(prefix='b2-mode-gates-')
        self.root = Path(self.tmp.name).resolve()
        self.apparatus = apparatus_hashes()
        self.source = {k: 'a' * 64 for k in ('node', 'baseline', 'node_modules', 'recorder_proxy')}
        self.meta = {'mode': 'direct', 'synthetic': False, 'run_id': 'test-only-1',
                     'fixture_path': '/isolated/same/direct', 'apparatus_sha256': self.apparatus,
                     'source_sha256': self.source, 'source_sha256_end': self.source,
                     'wall_start_ms': 1791936001000, 'wall_end_ms': 1791936002000}
        self.audit = {'processes': [{'name': 'recorder', 'pid': 101, 'exitCode': 0}],
                      'owned_groups_remaining': [], 'cleanup_errors': []}
        self.enum = {'appCaptured': True, 'routes': [], 'uniqueRouteCount': 0}
        self.first = self.write('one', self.meta)

    def tearDown(self):
        self.tmp.cleanup()

    def write(self, name, meta):
        folder = self.root / name / 'direct'
        folder.mkdir(parents=True, exist_ok=True)
        start, end = meta['wall_start_ms'], meta['wall_end_ms']
        evidence = clock_evidence(start, end) if type(start) is int and type(end) is int and 0 < end - start < 10000 else {}
        meta = {**meta, 'clock_regression_check': evidence}
        (folder / 'run-meta.json').write_text(json.dumps(meta))
        (folder / 'audit.json').write_text(json.dumps(self.audit))
        (folder / 'recording.jsonl').write_text(name)
        enum = folder.parent / 'enumeration'
        enum.mkdir(exist_ok=True)
        (enum / 'router-enumeration.json').write_text(json.dumps(self.enum))
        return folder

    def test_capture_identity_rejects_invalid_provenance(self):
        self.assertEqual(capture_identity(self.first, 'direct', self.apparatus)['capture_utc_day'], '2026-10-14')
        for key, value in [('synthetic', True), ('mode', 'proxy'), ('apparatus_sha256', {}),
                           ('source_sha256', {}), ('source_sha256_end', {}),
                           ('wall_start_ms', True), ('wall_end_ms', 1792022400001),
                           ('run_id', ''), ('fixture_path', 'relative')]:
            with self.subTest(key=key):
                meta = {**self.meta, key: value}
                self.write('one', meta)
                with self.assertRaises(ValueError):
                    capture_identity(self.first, 'direct', self.apparatus)

    def test_legacy_source_binding_is_only_allowed_for_historical_replay(self):
        source = {k: v for k, v in self.source.items() if k != 'node_modules'}
        self.write('one', {**self.meta, 'source_sha256': source, 'source_sha256_end': source})
        with self.assertRaisesRegex(ValueError, 'incomplete or changed'):
            capture_identity(self.first, 'direct', self.apparatus)
        self.assertEqual(capture_identity(self.first, 'direct', self.apparatus, historical=True)['source_sha256'], source)

    def test_copied_identity_cross_day_source_path_and_interval_rejected(self):
        pool = ReferencePool.__new__(ReferencePool)
        pool.apparatus, pool.enumeration = self.apparatus, self.enum
        pool.recorded_apparatus = None
        pool.identities = [capture_identity(self.first, 'direct', self.apparatus)]
        self.audit['processes'][0]['pid'] = 102
        base = {**self.meta, 'run_id': 'test-only-2', 'wall_start_ms': 1791936003000, 'wall_end_ms': 1791936004000}
        for field, value in [('run_id', self.meta['run_id']), ('fixture_path', '/different/direct'),
                             ('wall_start_ms', self.meta['wall_start_ms']),
                             ('source_sha256', {**self.source, 'node': 'b' * 64}),
                             ('day', 86400000), ('copy', None)]:
            with self.subTest(field=field):
                meta = copy.deepcopy(base)
                if field == 'day':
                    meta['wall_start_ms'] += value
                    meta['wall_end_ms'] += value
                elif field != 'copy':
                    meta[field] = value
                if field == 'source_sha256':
                    meta['source_sha256_end'] = value
                other = self.write('two', meta)
                if field == 'copy':
                    (other / 'recording.jsonl').write_bytes((self.first / 'recording.jsonl').read_bytes())
                with self.assertRaises(ValueError):
                    pool.admit(other, 'direct')
        other = self.write('two', base)
        pool.admit(other, 'direct')
        self.assertEqual(len(pool.identities), 2)

    def test_pool_insufficient_and_duplicate_paths_refused_before_read(self):
        for paths in ([self.first] * 20, [self.first] * 19):
            report = compare_with_pool(self.first, self.first, self.enum, paths)
            self.assertEqual(report['verdict'], 'REFUSED')

    def test_nontransitive_bridge_singleton_and_one_class_refused(self):
        def relation(groups):
            return {(i, j): {'verdict': 'PASS' if groups[i] == groups[j] else 'FAIL'}
                    for i in range(len(groups)) for j in range(i+1, len(groups))}
        for groups in ([0, 0, 0], [0, 0, 1]):
            with self.assertRaises(ValueError):
                confirmed_classes(len(groups), relation(groups))
        pairs = relation([0, 0, 1, 1])
        pairs[(1, 2)] = {'verdict': 'PASS'}
        with self.assertRaises(ValueError):
            confirmed_classes(4, pairs)
        self.assertEqual([g['count'] for g in confirmed_classes(4, relation([0, 0, 1, 1]))], [2, 2])

    def test_capture_cleanup_and_pid_are_mandatory(self):
        for key, value in [('owned_groups_remaining', [101]), ('cleanup_errors', ['error']),
                           ('processes', []), ('processes', [{'name': 'recorder', 'pid': True, 'exitCode': 0}]),
                           ('processes', [{'name': 'recorder', 'pid': 101, 'exitCode': 1}])]:
            with self.subTest(key=key, value=value):
                (self.first / 'audit.json').write_text(json.dumps({**self.audit, key: value}))
                with self.assertRaises(ValueError):
                    capture_identity(self.first, 'direct', self.apparatus)

    def test_no_candidate_reads_when_pool_underfilled(self):
        with patch('multimodal.capture_identity', side_effect=AssertionError('must not read candidates')):
            report = compare_with_pool(self.root / 'absent-left', self.root / 'absent-proxy', self.enum, [self.first])
        self.assertEqual(report['verdict'], 'REFUSED')

    def test_replay_refused_report_binds_full_original_apparatus(self):
        prior = {'k': 5, 'comparison_mode': 'calibration', 'verdict': 'REFUSED',
                 'apparatus_sha256': self.apparatus, 'pairs': []}
        path = self.root / 'original.json'
        path.write_text(json.dumps(prior))
        provenance = pool_replay_provenance(path)
        self.assertEqual(provenance['recorded_apparatus_sha256'], self.apparatus)
        self.assertEqual(provenance['changed_apparatus'], [])
        # Replay accepts the original hash map only; it cannot waive a mismatch.
        wrong = dict(self.apparatus)
        wrong[next(iter(wrong))] = 'b' * 64
        with self.assertRaises(ValueError):
            capture_identity(self.first, 'direct', wrong)
        for key, value in [('k', 4), ('comparison_mode', 'historical-replay'),
                           ('apparatus_sha256', {'incomplete': 'a' * 64}),
                           ('verdict', 'REPLAY_ONLY')]:
            with self.subTest(key=key):
                path.write_text(json.dumps({**prior, key: value}))
                with self.assertRaises(ValueError):
                    pool_replay_provenance(path)

    def test_unit_selectors_have_exact_event_ordinals_and_no_sibling_paths(self):
        side = SimpleNamespace(ws={}, types={})
        for key in ('ws.chat.2#0', 'ws.chat.3#0'):
            frames = [{'dir': 'u2c', 'type': 'projects_updated', 'opcode': 1, 'fin': True,
                       'body': {'type': 'projects_updated', 'changedFile': file, 'changeType': kind}}
                      for file, kind in [('first.jsonl', 'change'), ('second.jsonl', 'add')]]
            side.ws[key] = frames
            side.types[key] = {'projects_updated': copy.deepcopy(frames)}
        values, paths = registered_fields(side)
        self.assertEqual(len(paths), 8)
        self.assertEqual(values['ws.chat.2#0'], {
            'projects_updated[0]': {'changeType': 'change'},
            'projects_updated[1]': {'changedFile': 'second.jsonl'}})
        self.assertIn(('ws.sequence', 'ws."ws.chat.2#0"[0].body.changeType'), paths)
        self.assertNotIn(('ws.sequence', 'ws."ws.chat.2#0"[0].body.changedFile'), paths)
        self.assertNotIn(('ws.sequence', 'ws."ws.chat.2#0"[1].body.changeType'), paths)
        first_values, first_paths = registered_fields(side, ('U1',))
        second_values, second_paths = registered_fields(side, ('U2',))
        self.assertEqual(len(first_paths), 4)
        self.assertEqual(len(second_paths), 4)
        self.assertFalse(first_paths & second_paths)
        self.assertEqual(first_paths | second_paths, paths)
        self.assertEqual(first_values['ws.chat.2#0'], {'projects_updated[0]': {'changeType': 'change'}})
        self.assertEqual(second_values['ws.chat.2#0'], {'projects_updated[1]': {'changedFile': 'second.jsonl'}})
        # Even a repeated mixed broadcast cannot become an admissible class.
        for ordinal, field, value in [(0, 'changeType', 'add'), (1, 'changedFile', 'other.jsonl')]:
            altered = copy.deepcopy(side)
            altered.ws['ws.chat.3#0'][ordinal]['body'][field] = value
            altered.types['ws.chat.3#0']['projects_updated'][ordinal]['body'][field] = value
            with self.assertRaisesRegex(ValueError, 'unequal values'):
                registered_fields(altered)
        for field, value in [('fin', False), ('opcode', 0)]:
            altered = copy.deepcopy(side)
            altered.ws['ws.chat.2#0'][0][field] = value
            with self.assertRaises(ValueError):
                registered_fields(altered)
        side.types['ws.chat.2#0']['projects_updated'][0]['body']['changeType'] = 'add'
        with self.assertRaises(ValueError):
            registered_fields(side)


if __name__ == '__main__':
    unittest.main()
