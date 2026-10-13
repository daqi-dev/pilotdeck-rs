#!/usr/bin/env python3
"""Offline regression controls for PR #13 review; never use the shared lock."""
from contextlib import redirect_stdout, redirect_stderr
from copy import deepcopy
import io
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import time
import unittest
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import g03_compare as G
import check_ruler
import round as R
from controls.run_all import rows, rec, terminal
import self_compare


class ReviewControls(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory(prefix='pdrs-review-r1-', dir=G.WORKTREE / '_tmp')
        self.root = Path(self.tmp.name)
        self.addCleanup(self.tmp.cleanup)

    def test_dead_pid_lock_takeover(self):
        child = subprocess.Popen([sys.executable, '-c', 'pass'])
        child.wait(timeout=10)
        lock = self.root / 'parity.lock'
        lock.mkdir()
        (lock / 'owner.json').write_text(json.dumps({'pid': child.pid, 'timestamp': time.time()}))
        events, stderr = [], io.StringIO()
        with redirect_stderr(stderr), R.parity_lock(lock, events):
            owner = json.loads((lock / 'owner.json').read_text())
            self.assertEqual(owner['pid'], os.getpid())
            self.assertLess(abs(time.time() - owner['timestamp']), 5)
        self.assertFalse(lock.exists())
        self.assertEqual(events[0]['reason'], 'dead_pid')
        self.assertIn('lock_takeover', stderr.getvalue())

    def test_expired_live_pid_lock_takeover(self):
        lock = self.root / 'parity.lock'
        lock.mkdir()
        (lock / 'owner.json').write_text(json.dumps({'pid': os.getpid(), 'timestamp': time.time() - 2401}))
        events = []
        with redirect_stderr(io.StringIO()), R.parity_lock(lock, events):
            self.assertEqual(events[0]['reason'], 'expired')
        self.assertFalse(lock.exists())

    def test_live_lock_waits_and_preserves_new_owner_on_release(self):
        lock = self.root / 'parity.lock'
        lock.mkdir()
        original = {'pid': os.getpid(), 'timestamp': time.time()}
        (lock / 'owner.json').write_text(json.dumps(original))
        events = []
        def release_after_wait(_):
            self.assertEqual(R.read_owner(lock), original)
            (lock / 'owner.json').unlink()
            lock.rmdir()
        with patch.object(R.time, 'sleep', side_effect=release_after_wait), redirect_stderr(io.StringIO()):
            with R.parity_lock(lock, events):
                replacement = dict(R.read_owner(lock), token='replacement-owner')
                (lock / 'owner.json').write_text(json.dumps(replacement))
        self.assertEqual(events, [])
        self.assertEqual(R.read_owner(lock), replacement)

    def test_timeout_records_blocked_and_releases_lock(self):
        baseline = self.root / 'baseline'
        binary = baseline / 'dist/src/cli/pilotdeck-agent-loop-sidecar.js'
        binary.parent.mkdir(parents=True)
        binary.write_text('timeout-control')
        worktree = self.root / 'worktree'
        (worktree / '_tmp').mkdir(parents=True)
        out_root = worktree / '_tmp/parity'
        lock = self.root / 'parity.lock'
        real_popen, real_lock = subprocess.Popen, R.parity_lock
        spawned = []
        def sleeping_popen(cmd, **kwargs):
            spawned.append(kwargs.get('start_new_session'))
            return real_popen([sys.executable, '-c', 'import time; print("before timeout", flush=True); time.sleep(10)'], **kwargs)
        def isolated_lock(**kwargs):
            return real_lock(lock, **kwargs)
        argv = ['round.py', '--side', 'ts', '--label', 'timeout', '--scenario', 'pure_text', '--timeout-seconds', '0.2']
        stdout = io.StringIO()
        started = time.monotonic()
        with patch.object(sys, 'argv', argv), patch.dict(os.environ, {'PDRS_OUT': str(out_root)}), \
             patch.object(G, 'WORKTREE', worktree), patch.object(G, 'BASELINE', baseline), \
             patch.object(G, 'scenario_catalog', return_value={'pure_text': {}}), \
             patch.object(R.shutil, 'which', return_value=sys.executable), \
             patch.object(R.subprocess, 'check_output', return_value='v-control\n'), \
             patch.object(R.subprocess, 'Popen', side_effect=sleeping_popen), \
             patch.object(R, 'parity_lock', side_effect=isolated_lock), redirect_stdout(stdout):
            self.assertEqual(R.main(), 1)
        run = json.loads((out_root / 'timeout/run.json').read_text())
        # 子进程在独立进程组里起，超时后整组被杀：远早于 sleep(10) 结束就返回
        self.assertEqual(spawned, [True])
        self.assertLess(time.monotonic() - started, 5)
        self.assertIn('timed out after 0.2', run['blocked'][0])
        self.assertEqual(run['status'], 'BLOCKED')
        self.assertFalse(run['valid_round'])
        self.assertFalse(lock.exists())
        self.assertIn('before timeout', (out_root / 'timeout/run.py.stdout').read_text())
        self.assertEqual(json.loads(stdout.getvalue())['status'], 'BLOCKED')

    def test_missing_run_ledger_fails_closed(self):
        directory = self.root / 'new-round'
        artifacts = directory / 'artifacts'
        artifacts.mkdir(parents=True)
        trace = artifacts / 'pure_text.pilotdeck-current-sidecar.jsonl'
        proof = rec('harness.proof', state='sidecar_identity', side='ts', sha256='sealed',
                    expectedSha256='sealed', spawnObserved=True)
        trace.write_text(''.join(json.dumps(r) + '\n' for r in rows(proof, terminal())))
        ledger = directory / 'run.json'
        ledger.write_text(json.dumps({'valid_round': True, 'side': 'ts', 'expected_sha256': 'sealed'}))
        with patch.object(G, 'scenario_catalog', return_value={'pure_text': None}):
            for supplied in (directory, artifacts):
                result = G.compare_directories(supplied, supplied)
                self.assertEqual(result['per_scenario']['pure_text']['verdict'], 'PASS')
            ledger.unlink()
            for supplied in (directory, artifacts):
                result = G.compare_directories(supplied, supplied)
                self.assertEqual(result['verdict'], 'FAIL')
                self.assertIn('trace_identity', result['per_scenario']['pure_text']['triggered_rules'])
            # A copied proof cannot downgrade the trace to a legacy flat capture.
            flat = self.root / 'flat'
            flat.mkdir()
            (flat / trace.name).write_text(trace.read_text())
            result = G.compare_directories(flat, flat)
            self.assertIn('trace_identity', result['per_scenario']['pure_text']['triggered_rules'])
            # Legacy upstream capture without identity proof remains supported.
            (flat / trace.name).write_text(''.join(json.dumps(r) + '\n' for r in rows(terminal())))
            result = G.compare_directories(flat, flat)
            self.assertEqual(result['per_scenario']['pure_text']['verdict'], 'PASS')

    def test_e12_exact_field_match(self):
        messages = [{'role': 'user', 'content': [{'type': 'tool_result', 'toolCallId': 'call-one'}]}]
        good = rows(rec('tool.call', toolCallId='call-one'), rec('model.request', modelView={'messages': messages}), terminal())
        self.assertEqual(G.e_rules(good, 'left', 'pure_text')['E12']['status'], 'PASS')
        for changed_messages in (
            [{'role': 'tool', 'toolCallId': 'call-one-longer'}],
            [{'role': 'tool', 'content': 'call-one'}],
        ):
            bad = deepcopy(good)
            bad[1]['modelView']['messages'] = changed_messages
            self.assertEqual(G.e_rules(bad, 'left', 'pure_text')['E12']['status'], 'FAIL')

    def test_worktree_defaults_and_baseline_fallback(self):
        self.assertEqual(self_compare.DEFAULT_SOURCE, G.WORKTREE / '_tmp/20261014-ts5')
        self.assertEqual(check_ruler.DEFAULT_SOURCE, G.WORKTREE / '_tmp/20261014-exp001/parity-run1')
        local = self.root / 'resources/PilotDeck'
        fallback = self.root / 'external-baseline'
        local.mkdir(parents=True)
        fallback.mkdir()
        # 显式 PDRS_BASELINE 优先；其次 worktree 的 resources/；都没有（且不在 git 仓里）就报错
        with patch.object(G, 'WORKTREE', self.root), patch.dict(os.environ, {'PDRS_BASELINE': str(fallback)}):
            self.assertEqual(G.baseline_path(), fallback.resolve())
            with patch.dict(os.environ, {'PDRS_BASELINE': ''}):
                self.assertEqual(G.baseline_path(), local.resolve())
                local.rmdir()
                not_a_repo = subprocess.CompletedProcess(args=[], returncode=128, stdout='', stderr='')
                with patch.object(G.subprocess, 'run', return_value=not_a_repo), \
                     self.assertRaisesRegex(FileNotFoundError, 'set PDRS_BASELINE'):
                    G.baseline_path()

    def test_malformed_self_comparison_returns_structured_failure(self):
        self.assertTrue(check_ruler.check_self_comparison(G.WORKTREE / '_tmp/self-compare.json')['ok'],
                        'regenerate self-compare.json after changes before running review controls')
        valid = json.loads((G.WORKTREE / '_tmp/self-compare.json').read_text())
        malformed = {'list': [], 'missing_input': dict(valid), 'missing_original': dict(valid),
                     'bad_pair': dict(valid), 'missing_verdict': deepcopy(valid)}
        del malformed['missing_input']['input_sha256']
        del malformed['missing_original']['original_sha256']
        malformed['bad_pair']['pairs'] = {key: [] for key in valid['pairs']}
        del malformed['missing_verdict']['pairs']['r1-r2']['per_scenario']['pure_text']['verdict']
        expected_errors = {'list': 'list indices', 'missing_input': 'input_sha256',
                           'missing_original': 'original_sha256', 'bad_pair': 'has no attribute',
                           'missing_verdict': 'verdict'}
        for name, value in malformed.items():
            with self.subTest(name=name):
                path = self.root / (name + '.json')
                path.write_text(json.dumps(value))
                result = check_ruler.check_self_comparison(path)
                self.assertFalse(result['ok'])
                self.assertIsInstance(result['error'], str)
                self.assertIn(expected_errors[name], result['error'])
                # Main must emit a failed JSON report and exit 1 too.
                argv = ['check_ruler.py', '--self-compare', str(path), '--json', str(self.root / 'check.json')]
                with patch.object(sys, 'argv', argv), \
                     patch.object(check_ruler, 'run_controls', return_value={'negative_controls': True, 'both_broken': True, 'positive_controls': True}), \
                     patch.object(check_ruler, 'check_frozen', return_value={'ok': True, 'fully_registered': True}), \
                     redirect_stdout(io.StringIO()):
                    self.assertEqual(check_ruler.main(), 1)
                report = json.loads((self.root / 'check.json').read_text())
                self.assertFalse(report['self_compare'])
                self.assertFalse(report['machine_checks_pass'])
                self.assertIsInstance(report['self_compare_error'], str)

    def test_unregistered_freeze_blocks_machine_pass(self):
        argv = ['check_ruler.py', '--json', str(self.root / 'freeze.json')]
        with patch.object(sys, 'argv', argv), \
             patch.object(check_ruler, 'check_self_comparison', return_value={'ok': True, 'S_size': 53, 'error': None}), \
             patch.object(check_ruler, 'run_controls', return_value={'negative_controls': True, 'both_broken': True, 'positive_controls': True}), \
             patch.object(check_ruler, 'check_frozen', return_value={'ok': True, 'fully_registered': False}), \
             redirect_stdout(io.StringIO()):
            self.assertEqual(check_ruler.main(), 1)
        result = json.loads((self.root / 'freeze.json').read_text())
        self.assertTrue(result['frozen_hashes'])
        self.assertFalse(result['fully_frozen'])
        self.assertFalse(result['machine_checks_pass'])


if __name__ == '__main__':
    unittest.main(verbosity=2)
