"""Calibration safety tests with fake runner output; never build or listen."""
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
from datetime import datetime, timezone
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

import calibrate
from calibrate import Batch, gate_report
from common import ClockRefused
from controls.run_all import clock_evidence


class CalibrationControls(unittest.TestCase):
    def test_exit_two_is_not_enough_to_pass_ruler_gate(self):
        good = dict(verdict='PENDING_REVIEW', calibration_eligible=True, comparison_mode='calibration',
                    self_compare=True, pairs=[{'verdict': 'PASS'} for _ in range(10)],
                    per_rule_negative=True, same_bad_rejected=True, errors=[], independent_review=None,
                    proxy_compare={'verdict': 'PASS'})
        gate_report(good, proxy=True)
        for mutation in ({'verdict': 'REFUSED'}, {'comparison_mode': 'historical-replay'},
                         {'pairs': [{'verdict': 'FAIL'}] * 10}, {'pairs': []},
                         {'proxy_compare': {'verdict': 'FAIL'}}, {'calibration_eligible': False}):
            with self.subTest(mutation=mutation), self.assertRaises(ValueError):
                gate_report({**good, **mutation}, proxy=True)

    def batch(self, tmp):
        root = Path(tmp)
        (root / 'attempts').mkdir()
        state = dict(head='test-head', baseline='/readonly/baseline', utc_day=calibrate.utc_day(), runs={},
                     pinned={'apparatus': {}, 'source': {'node': 'a' * 64, 'baseline': 'b' * 64,
                                                       'recorder_proxy': 'c' * 64}})
        return Batch(root, state)

    def fake_run(self, command, **kwargs):
        slot = Path(command[command.index('--out') + 1])
        (slot / 'direct').mkdir(parents=True)
        (slot / 'direct/run-meta.json').write_text('{"clock_regression_check":{"status":"REFUSED"}}')
        (slot / 'result.json').write_text('{"status":"REFUSED"}')
        return type('Result', (), {'returncode': 2})()

    def test_refused_run_archived_and_only_named_slot_retried(self):
        with tempfile.TemporaryDirectory() as tmp:
            batch = self.batch(tmp)
            with patch.object(batch, 'guard'), patch('calibrate.subprocess.run', side_effect=self.fake_run) as run:
                for retry in (False, True):
                    with self.assertRaisesRegex(ValueError, 'retry ONLY this run'):
                        batch.capture('ref1', retry=retry)
                self.assertEqual(run.call_count, 2)
            self.assertEqual(set(batch.state['runs']), {'ref1'})
            self.assertEqual(batch.state['runs']['ref1']['status'], 'REFUSED')
            attempts = batch.state['attempts']
            self.assertNotEqual(attempts[0]['archive'], attempts[1]['archive'])
            for entry in attempts:
                self.assertEqual(json.loads((Path(entry['archive']) / 'result.json').read_text())['status'], 'REFUSED')
            self.assertFalse(batch.slot.exists())

    def test_success_is_archived_once_and_resume_never_recaptures_it(self):
        with tempfile.TemporaryDirectory() as tmp:
            batch = self.batch(tmp)

            def success(command, **kwargs):
                slot = Path(command[command.index('--out') + 1])
                (slot / 'direct').mkdir(parents=True)
                (slot / 'result.json').write_text('{"status":"PASS"}')
                return type('Result', (), {'returncode': 0})()

            with patch.object(batch, 'guard'), patch.object(batch, 'admit', return_value={'run_id': 'fresh'}), \
                    patch('calibrate.subprocess.run', side_effect=success) as run:
                batch.capture('ref1')
                batch.capture('ref1')
                self.assertEqual(run.call_count, 1)
            self.assertEqual(batch.state['attempts'][0]['archive'], batch.state['runs']['ref1']['archive'])
            self.assertTrue(Path(batch.state['attempts'][0]['archive']).is_dir())
            self.assertFalse(batch.slot.exists())

    def test_retry_cannot_replace_pool_after_scoring_or_overwrite_pass(self):
        with tempfile.TemporaryDirectory() as tmp:
            batch = self.batch(tmp)
            batch.state['runs'] = {'ref1': {'status': 'REFUSED'}, 'ts1': {'status': 'PASS'}}
            with self.assertRaisesRegex(ValueError, 'pool already consumed'):
                batch.capture('ref1', retry=True)
            batch.state['runs']['ref1']['status'] = 'PASS'
            with self.assertRaisesRegex(ValueError, 'exact run'):
                batch.capture('ref1', retry=True)
            with self.assertRaisesRegex(ValueError, 'earlier slots'):
                batch.capture('ref3')

    def test_resume_rejects_day_or_source_change(self):
        with tempfile.TemporaryDirectory() as tmp:
            batch = self.batch(tmp)
            with patch('calibrate.git_admission'), patch.object(batch, 'verify_build'), patch('calibrate.utc_day', return_value='other-day'), self.assertRaisesRegex(ValueError, 'UTC midnight'):
                batch.guard()
            with patch('calibrate.git_admission'), patch.object(batch, 'verify_build'), patch.object(batch, 'pin', return_value={}), self.assertRaisesRegex(ValueError, 'source changed'):
                batch.guard()

    def test_pass_capture_with_missing_or_bad_clock_is_not_admitted(self):
        with tempfile.TemporaryDirectory() as tmp:
            batch = self.batch(tmp)
            directory = Path(tmp) / 'ref1/direct'
            directory.mkdir(parents=True)
            for meta in ({}, {'clock_regression_check': {'schema': 1, 'status': 'REFUSED', 'reason': 'wide bracket'}}):
                (directory / 'run-meta.json').write_text(json.dumps(meta))
                with self.assertRaises(ClockRefused):
                    batch.admit('ref1')

    def test_source_path_day_and_overlap_are_checked_on_admission(self):
        with tempfile.TemporaryDirectory() as tmp:
            batch = self.batch(tmp)
            directory = Path(tmp) / 'ref2/direct'
            directory.mkdir(parents=True)
            start = int(datetime.now(timezone.utc).timestamp() * 1000)
            meta = dict(baseline_path=batch.state['baseline'], wall_start_ms=start, wall_end_ms=start + 1000,
                        clock_regression_check=clock_evidence(start, start + 1000))
            (directory / 'run-meta.json').write_text(json.dumps(meta))
            identity = dict(source_sha256=batch.state['pinned']['source'], capture_utc_day=batch.state['utc_day'],
                            fixture_path=str(batch.slot / 'direct'), run_id='fresh', recorder_pid=200,
                            recording_sha256='e' * 64, wall_start_ms=start, wall_end_ms=start + 1000)
            with patch('calibrate.capture_identity', return_value=identity):
                self.assertEqual(batch.admit('ref2'), identity)
            for mutation in ({'capture_utc_day': 'other-day'}, {'fixture_path': '/another/slot'},
                             {'source_sha256': {}}):
                with patch('calibrate.capture_identity', return_value={**identity, **mutation}), self.assertRaises(ValueError):
                    batch.admit('ref2')
            old = {**identity, 'run_id': 'previous', 'recorder_pid': 100, 'recording_sha256': 'f' * 64}
            batch.state['runs']['ref1'] = {'status': 'PASS', 'identity': old}
            with patch('calibrate.capture_identity', return_value=identity), self.assertRaisesRegex(ValueError, 'overlap'):
                batch.admit('ref2')

    def test_space_preflight_uses_projected_total_and_disk_free(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            (root / 'reference/direct').mkdir(parents=True)
            (root / 'reference/direct/recording.jsonl').write_text('x')
            free = type('Disk', (), {'free': 50_000_000_000})()
            with patch('calibrate.size', side_effect=[12_000_000, 29_000_000_000]), \
                    patch('calibrate.shutil.disk_usage', return_value=free), self.assertRaisesRegex(ValueError, 'space budget'):
                calibrate.space_check(root, root / 'reference')
            with patch('calibrate.size', side_effect=[12_000_000, 16_000_000_000]), \
                    patch('calibrate.shutil.disk_usage', return_value=free):
                report = calibrate.space_check(root, root / 'reference')
            self.assertEqual(report['projected_bytes'], 18_468_000_000)

    def test_fresh_target_build_ignores_forged_old_binary_and_detects_new_drift(self):
        import os
        import shutil
        import subprocess
        import hashlib
        if not shutil.which('cargo'):
            self.skipTest('Cargo unavailable')
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp).resolve()
            manifest = root / 'scripts/exp008/proxy/Cargo.toml'
            manifest.parent.mkdir(parents=True)
            manifest.write_text('[package]\nname="pdrs-exp008-proxy"\nversion="0.1.0"\nedition="2021"\n')
            source = manifest.parent / 'src/main.rs'
            source.parent.mkdir()
            source.write_text('fn main() { println!("FRESH"); }\n')
            old_target = root / '_build/exp010-target'
            env = dict(os.environ, CARGO_HOME=str(root / '_build/cargo-home'), CARGO_TARGET_DIR=str(old_target))
            subprocess.run(['cargo', 'generate-lockfile', '--offline', '--manifest-path', str(manifest)],
                           env=env, check=True, capture_output=True)
            subprocess.run(['cargo', 'build', '--locked', '--offline', '--manifest-path', str(manifest)],
                           env=env, check=True, capture_output=True)
            old_binary = old_target / 'debug/pdrs-exp008-proxy'
            # Cargo may copy the deps executable back to debug even when fresh.
            # Forge both outputs without changing the source/fingerprint timestamps.
            outputs = [old_binary] + [p for p in (old_target / 'debug/deps').glob('pdrs_exp008_proxy-*')
                                      if p.suffix == '']
            for output in outputs:
                old_stat = output.stat()
                output.write_text('#!/bin/sh\necho FORGED\n')
                output.chmod(0o755)
                os.utime(output, ns=(old_stat.st_atime_ns, old_stat.st_mtime_ns))
            subprocess.run(['cargo', 'build', '--locked', '--offline', '--manifest-path', str(manifest)],
                           env=env, check=True, capture_output=True)
            self.assertEqual(subprocess.check_output([str(old_binary)], text=True).strip(), 'FORGED')
            batch_path = root / '_tmp/batch'
            batch_path.mkdir(parents=True)
            batch = self.batch(batch_path)
            batch.state.update(resource_root=str(root), size_reference=str(root))
            with patch.object(calibrate, 'ROOT', root), patch('calibrate.space_check'):
                batch.build()
                batch.verify_build()
                self.assertEqual(subprocess.check_output([str(batch.binary)], text=True).strip(), 'FRESH')
                self.assertEqual(batch.state['build']['sha256'], hashlib.sha256(batch.binary.read_bytes()).hexdigest())
                self.assertNotEqual(batch.state['build']['sha256'], hashlib.sha256(old_binary.read_bytes()).hexdigest())
                self.assertEqual(batch.state['build']['target_dir'], str(batch_path / 'cargo-target'))
                with self.assertRaisesRegex(ValueError, 'target already exists'):
                    batch.build()
                original = batch.binary.read_bytes()
                batch.binary.write_bytes(b'FORGED')
                with self.assertRaisesRegex(ValueError, 'build missing/changed'):
                    batch.verify_build()
                batch.binary.write_bytes(original)
                batch.state['build'].update(target_dir=str(old_target), binary=str(old_binary),
                                             sha256=hashlib.sha256(old_binary.read_bytes()).hexdigest())
                with self.assertRaisesRegex(ValueError, 'build missing/changed'):
                    batch.verify_build()
            print('CONTROL Cargo old target=FORGED; fresh batch target=FRESH; old/reused/changed batch build=FAIL')

    def test_batch_guard_refuses_HEAD_change_before_capture(self):
        from apparatus import ApparatusUnsafe
        with tempfile.TemporaryDirectory() as tmp:
            batch = self.batch(tmp)
            with patch('calibrate.git_admission', side_effect=ApparatusUnsafe('batch HEAD changed')), \
                    patch.object(batch, 'pin') as pin, self.assertRaisesRegex(ValueError, 'HEAD changed'):
                batch.guard()
            pin.assert_not_called()

    def test_integrity_admission_failure_reports_FAIL_exit_1(self):
        import contextlib
        import io
        from apparatus import ApparatusUnsafe
        output = io.StringIO()
        with patch('sys.argv', ['calibrate.py', '--preflight']), \
                patch('calibrate.git_admission', side_effect=ApparatusUnsafe('dirty apparatus')), \
                contextlib.redirect_stderr(output):
            self.assertEqual(calibrate.main(), 1)
        self.assertEqual(json.loads(output.getvalue())['verdict'], 'FAIL')
