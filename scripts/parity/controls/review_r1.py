#!/usr/bin/env python3
"""Offline regression controls for PR #13 review; never use the shared lock."""
import sys

# -I keeps PYTHONPATH, user site and the script directory out of module
# resolution; without it, exit before loading any apparatus module.
if not sys.flags.isolated:
    raise SystemExit(f'run with -I: python3 -I {__file__}')

if sys.version_info < (3, 11) or 'os' not in sys.modules:
    raise SystemExit('os must be preloaded from CPython 3.11+ frozen modules')
import os

_boot_dir = os.path.dirname(os.path.realpath(__file__))
_boot_dir = os.path.dirname(_boot_dir) if os.path.basename(_boot_dir) == 'controls' else _boot_dir
# Identity, not spelling: a case-insensitive alias, a symlinked spelling or a
# mixed ``//`` form names this very directory while the string differs
# (#1033). Nonexistent sys.path entries skip; an empty entry means cwd.
if any(
        os.path.isdir(entry or os.curdir)
        and any(os.path.isdir(root)
                and os.path.samefile(entry or os.curdir, root)
                for root in (_boot_dir, os.path.join(_boot_dir, 'controls')))
        for entry in sys.path):
    raise SystemExit('apparatus directory on sys.path; run the entry file directly with -I')

import tempfile
sys.pycache_prefix = tempfile.mkdtemp(prefix='pdrs-parity-pyc-')
sys.dont_write_bytecode = True

from contextlib import redirect_stdout, redirect_stderr
from copy import deepcopy
import importlib.util
import io
import json
import os
from pathlib import Path
import subprocess
import time
import unittest
from unittest.mock import patch

_HERE = Path(__file__).resolve().parent
_ROOT = _HERE.parent


def _load(name, path):
    """Load an apparatus module by explicit path; never resolve via sys.path."""
    path = Path(path).resolve()
    existing = sys.modules.get(name)
    existing_file = getattr(existing, '__file__', None)
    if existing is not None and existing_file and Path(existing_file).resolve() == path:
        return existing
    spec = importlib.util.spec_from_file_location(name, path)
    module = importlib.util.module_from_spec(spec)
    sys.modules[name] = module
    spec.loader.exec_module(module)
    if Path(module.__file__).resolve() != path:
        raise RuntimeError(f'{name}: loaded {module.__file__}, expected {path}')
    return module


G = _load('g03_compare', _ROOT / 'g03_compare.py')
check_ruler = _load('check_ruler', _ROOT / 'check_ruler.py')
R = _load('round', _ROOT / 'round.py')
_run_all = _load('controls.run_all', _HERE / 'run_all.py')
rows, rec, terminal = _run_all.rows, _run_all.rec, _run_all.terminal
self_compare = _load('self_compare', _ROOT / 'self_compare.py')


class ReviewControls(unittest.TestCase):
    def setUp(self):
        # _tmp/ is gitignored, so a fresh checkout has no such directory yet;
        # TemporaryDirectory(dir=...) refuses to create the parent itself.
        (G.WORKTREE / '_tmp').mkdir(parents=True, exist_ok=True)
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
                # Main must emit a failed JSON report and exit 1 too. #2398
                # fix-r1: every subTest gets its own --json path — the result
                # file is created exclusively, so a shared path would make the
                # second subTest die on "already exists" before asserting.
                report_path = self.root / (name + '.check.json')
                argv = ['check_ruler.py', '--self-compare', str(path), '--json', str(report_path)]
                with patch.object(sys, 'argv', argv), \
                     patch.object(check_ruler, 'run_controls', return_value={'negative_controls': True, 'both_broken': True, 'positive_controls': True}), \
                     patch.object(check_ruler, 'check_frozen', return_value={'ok': True, 'fully_registered': True}), \
                     redirect_stdout(io.StringIO()):
                    self.assertEqual(check_ruler.main(), 1)
                report = json.loads(report_path.read_text())
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

    def test_independent_review_disposal_matrix(self):
        """#2245: the conclusion sentence plus a fully-disposed table must pass;
        un-disposed, in-progress or unreadable P0/P1/P2 rows must fail."""
        record_dir = self.root / 'docs' / 'reviews'
        record_dir.mkdir(parents=True)
        record = record_dir / '001_ruler-recheck.md'
        table = ('# 记录\n\n| 轮 | 审查方 | 结论 | 处理 |\n|---|---|---|---|\n'
                 '| 1 | DS | P1：foo | 已修（c1） |\n')
        conclusion = '\n结论：**无 P0/P1/P2。**\n'
        # (name, row to replace the closed row with, expected independent_review)
        row_cases = [
            ('closed', '| 1 | DS | P1：foo | 已修（c1） |\n', True),
            ('undisposed-dash', '| 1 | DS | P1：foo | — |\n', False),
            ('in-progress', '| 1 | DS | P1：foo | 修复中 |\n', False),
            ('planned-fix', '| 1 | DS | P1：foo | 计划修复 |\n', False),
            ('pending-verb', '| 1 | DS | P1：foo | 待 codex 修 |\n', False),
            ('english-not-fixed', '| 1 | DS | P1: foo | not fixed |\n', False),
            ('english-unresolved', '| 1 | DS | P1: foo | unresolved |\n', False),
            ('keep-observing', '| 1 | DS | P1：foo | 保留观察 |\n', False),
            ('escaped-pipe-hides-p1', '| 1 | DS \\| GLM | P1：foo | 待修复 |\n', False),
            ('escaped-pipe-clean', '| 1 | DS \\| GLM | 无 P0/P1/P2 | 已修（c1） |\n', True),
            ('closed-with-未-elsewhere', '| 1 | DS | P1：foo | 已修，未引入回归 |\n', True),
            ('merged-to-main', '| 1 | DS | P1：foo | 已合入 main（abc1234） |\n', True),
            ('folded-into-issue', '| 1 | DS | P1：foo | 已并入 #1234 |\n', True),
            ('downgraded', '| 1 | DS | P1：foo | 已降级（P3） |\n', True),
            ('negated-lowercase', '| 1 | DS | 无 p0/p1/p2 | — |\n', True),
            ('negated-fullwidth-slash', '| 1 | DS | 无 P0／P1 | — |\n', True),
            ('pure-negation-row', '| 1 | DS | 无 P0/P1/P2 | — |\n', True),
            ('misaligned-row', '| 1 | DS | P1：foo | 已修（c1） | extra |\n', False),
            ('pending-merge', '| 1 | DS | P1：foo | 待合入 main |\n', False),
            ('pending-merged-but', '| 1 | DS | P1：foo | 已修但未合入 |\n', False),
            ('pending-incomplete', '| 1 | DS | P1：foo | 未完成 |\n', False),
            ('pending-fold', '| 1 | DS | P1：foo | 尚未并入 #1234 |\n', False),
            ('pending-dated-plan', '| 1 | DS | P1：foo | 计划 2026 年 12 月修复 |\n', False),
            ('english-not-done', '| 1 | DS | P1: foo | not done |\n', False),
            ('english-not-resolved', '| 1 | DS | P1: foo | not resolved |\n', False),
            ('english-unfixed', '| 1 | DS | P1: foo | unfixed |\n', False),
            ('english-not-merged', '| 1 | DS | P1: foo | not merged |\n', False),
            ('pending-not-fixed', '| 1 | DS | P1: foo | 没修 |\n', False),
            ('pending-fix-awaiting-review', '| 1 | DS | P1：foo | 修复待评审 |\n', False),
            ('pending-awaiting-acceptance', '| 1 | DS | P1：foo | 已修，待人工验收 |\n', False),
            ('identifier-not-level', '| 1 | DS | case_p1 复核 | 已修（c1） |\n', True),
            ('closed-level-in-handling', '| 1 | DS | P1：foo | 已修（原 P1，降级 P3） |\n', True),
        ]
        for name, row, expected in row_cases:
            with self.subTest(name=name):
                record.write_text(table.replace('| 1 | DS | P1：foo | 已修（c1） |\n', row) + conclusion,
                                  encoding='utf-8')
                with patch.object(G, 'WORKTREE', self.root):
                    self.assertIs(check_ruler.independent_review(), expected)
        # whole-record cases: table shape, fences, second tables, endings.
        findings_table = '\n| 文件 | 结论 |\n|---|---|\n| x.py | P2 遗留未修 |\n'
        unrelated_table = '\n| 名 | 值 |\n|---|---|\n| a | b |\n'
        combined_header_table = ('\n| 角色 | 不变量 | 结论 / 处理 | 证据 |\n'
                                 '|---|---|---|---|\n| a | b | P1：未修 | c |\n')
        fenced_example = '```\n| a | b |\n|---|---|\n| 1 | 2 |\n```\n\n'
        record_cases = [
            ('no-table', '# 记录\n' + conclusion, False),
            ('no-conclusion', table, False),
            ('fence-before-table', fenced_example + table + conclusion, True),
            ('fenced-poison-ignored', table + conclusion
             + '```\n| 结论 | 处理 |\n|---|---|\n| P1：x | — |\n```\n', True),
            ('second-findings-table', table + conclusion + findings_table, False),
            ('second-unrelated-table', table + conclusion + unrelated_table, True),
            ('combined-header-findings-table', table + conclusion + combined_header_table, False),
            ('dropped-leading-pipe', table + '2 | DS | P2：b | — |\n' + conclusion, False),
            ('cr-only-endings', (table + conclusion).replace('\n', '\r'), True),
            ('cr-only-poison', ((table + '| 1 | DS | P1：foo | — |\n') + conclusion)
             .replace('\n', '\r'), False),
            ('unclosed-fence-hides-poison',
             table + conclusion + '```python\n| 轮 | 结论 | 处理 |\n'
             '|---|---|---|\n| 2 | P1：x | — |\n', False),
            ('conclusion-only-inside-fence',
             table + '```\n结论：**无 P0/P1/P2。**\n```\n', False),
            ('level-in-separate-column',
             '# 记录\n\n| 轮 | 等级 | 结论 | 处理 |\n|---|---|---|---|\n'
             '| 2 | P1 | foo | — |\n' + conclusion, False),
            ('adjacent-tables-no-blank-line',
             table + '| 项 | 描述 | 处理 |\n|---|---|---|\n'
             '| 5 | 某问题 | P1 未处置 |\n' + conclusion, False),
            ('fullwidth-level-poison',
             '# 记录\n\n| 轮 | 审查方 | 结论 | 处理 |\n|---|---|---|---|\n'
             '| 1 | DS | Ｐ1：foo | — |\n' + conclusion, False),
            ('fullwidth-digit-poison',
             '# 记录\n\n| 轮 | 审查方 | 结论 | 处理 |\n|---|---|---|---|\n'
             '| 1 | DS | P１：foo | — |\n' + conclusion, False),
            ('second-review-table-clean',
             table + '\n| 轮 | 审查方 | 结论 | 处理 |\n|---|---|---|---|\n'
             '| 2 | MM | 无 P0/P1/P2 | 已复核 |\n' + conclusion, True),
        ]
        for name, body, expected in record_cases:
            with self.subTest(name=name):
                record.write_text(body, encoding='utf-8')
                with patch.object(G, 'WORKTREE', self.root):
                    self.assertIs(check_ruler.independent_review(), expected)
        record.write_bytes(b'# \xff\xfe not utf-8 \n')
        with patch.object(G, 'WORKTREE', self.root):
            self.assertIs(check_ruler.independent_review(), False)
        # The real in-repo record must stay clean under the tightened rule:
        # assert on a byte snapshot, not the live path (r2 concurrency review).
        (record_dir / '001_ruler-recheck.md').write_bytes(
            (G.WORKTREE / 'docs/reviews/001_ruler-recheck.md').read_bytes())
        with patch.object(G, 'WORKTREE', self.root):
            self.assertIs(check_ruler.independent_review(), True)


if __name__ == '__main__':
    unittest.main(verbosity=2)
