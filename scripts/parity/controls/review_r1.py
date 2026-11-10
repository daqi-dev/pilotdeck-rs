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
import signal
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
            # fix-r4: a failed or still-expected disposal reads as pending.
            ('fix-failed', '| 1 | DS | P1：foo | 修复失败 |\n', False),
            ('solve-failed', '| 1 | DS | P1：foo | 解决失败 |\n', False),
            ('merge-failed', '| 1 | DS | P1：foo | 合入失败 |\n', False),
            ('pending-dated-plan-31', '| 1 | DS | P1：foo | 计划 2026 年 12 月 31 日修复 |\n', False),
            ('pending-paren-round', '| 1 | DS | P1：foo | 待下轮（r3）修复 |\n', False),
            ('english-not-yet-fixed', '| 1 | DS | P1: foo | not yet fixed |\n', False),
            ('english-never-fixed', '| 1 | DS | P1: foo | never fixed |\n', False),
            ('english-has-not-been-fixed', '| 1 | DS | P1: foo | has not been fixed |\n', False),
            # fix-r4: merged closes like 合入 (r3 reviews, both families).
            ('english-merged', '| 1 | DS | P1: foo | merged to main |\n', True),
            ('chinese-merged', '| 1 | DS | P1：foo | 已合并 to main |\n', True),
            ('english-done', '| 1 | DS | P1: foo | done |\n', True),
            ('english-fixed', '| 1 | DS | P1: foo | fixed |\n', True),
            ('english-resolved', '| 1 | DS | P1: foo | resolved |\n', True),
            ('english-closed', '| 1 | DS | P1: foo | closed |\n', True),
            ('english-wontfix', '| 1 | DS | P1: foo | wontfix |\n', True),
            ('failed-but-retried', '| 1 | DS | P1：foo | 已修复，失败率 0 |\n', True),
            # fix-r5: negated closure forms must read as pending (both families).
            ('english-unmerged', '| 1 | DS | P1: foo | unmerged |\n', False),
            ('english-unclosed', '| 1 | DS | P1: foo | unclosed |\n', False),
            ('english-un-fixed', '| 1 | DS | P1: foo | un-fixed |\n', False),
            ('english-un-fixed-underscore', '| 1 | DS | P1: foo | un_fixed |\n', False),
            ('english-hasnt-been-fixed', "| 1 | DS | P1: foo | hasn't been fixed |\n", False),
            ('english-wasnt-merged', "| 1 | DS | P1: foo | wasn't merged |\n", False),
            ('chinese-无法修复', '| 1 | DS | P1：foo | 无法修复 |\n', False),
            ('chinese-不能修复', '| 1 | DS | P1：foo | 不能修复 |\n', False),
            ('chinese-推迟修复', '| 1 | DS | P1：foo | 推迟修复 |\n', False),
            ('chinese-延后修复', '| 1 | DS | P1：foo | 延后修复 |\n', False),
            ('chinese-合并失败', '| 1 | DS | P1：foo | 合并失败 |\n', False),
            ('chinese-并入失败', '| 1 | DS | P1：foo | 并入失败 |\n', False),
            ('chinese-完成失败', '| 1 | DS | P1：foo | 完成失败 |\n', False),
            ('chinese-关闭失败', '| 1 | DS | P1：foo | 关闭失败 |\n', False),
            ('chinese-降级失败', '| 1 | DS | P1：foo | 降级失败 |\n', False),
            # fix-r5: cross-language pairs bind (未 merged / 待 done).
            ('cross-未-merged', '| 1 | DS | P1: foo | 未 merged |\n', False),
            ('cross-待-done', '| 1 | DS | P1: foo | 待 done |\n', False),
            ('cross-未-closed', '| 1 | DS | P1: foo | 未 closed |\n', False),
            ('cross-未-resolved', '| 1 | DS | P1: foo | 未 resolved |\n', False),
            # fix-r5: intention markers look forward across clauses; negation
            # markers stay within theirs (no character window left).
            ('intent-across-comma', '| 1 | DS | P1：foo | 计划 2026 年 12 月 31 日，修复 |\n', False),
            ('deferred-with-plan-ref', '| 1 | DS | P1：foo | 暂缓，修复方案见 #100 |\n', False),
            ('intent-long-gap', '| 1 | DS | P1：foo | 计划在 2026 年 12 月 31 日之后的第二个大版本发布窗口期修复 |\n', False),
            ('negation-stays-in-clause', '| 1 | DS | P1：foo | 已修，未引入回归 |\n', True),
            ('no-findings-then-fixed', '| 1 | DS | P1：foo | 没发现问题，已修复 |\n', True),
            # fix-r5: accepted/Reviewed dispositions close; won't fix is wontfix.
            ('accepted', '| 1 | DS | P1：foo | 已验收 |\n', True),
            ('re-reviewed', '| 1 | DS | P1：foo | 已复核 |\n', True),
            ('wont-fix-spelled-out', "| 1 | DS | P1: foo | won't fix |\n", True),
            # fix-r5: level negation tolerates short modifiers.
            ('negated-with-modifier', '| 1 | DS | 无新增 P0/P1/P2 | — |\n', True),
            ('negated-any-modifier', '| 1 | DS | 未发现任何 P0/P1/P2 | — |\n', True),
            # fix-r7: level negation is grammar — negation + a bounded gap
            # that excludes verb-initial characters, so a level following
            # verb-headed prose survives instead of folding away (r5/r6 P0).
            ('negation-grammar-cannot-confirm',
             '| 1 | DS | 无法确认 P1 是否已修 | — |\n', False),
            ('negation-grammar-cannot-repro',
             '| 1 | DS | 无法复现 P1 | — |\n', False),
            ('negation-grammar-unseen-fix',
             '| 1 | DS | 未见修复 P1：锁无过期回收 | — |\n', False),
            ('negation-grammar-unseen-xiu',
             '| 1 | DS | 未见修 P1 | — |\n', False),
            ('negation-grammar-not-handled',
             '| 1 | DS | 没有处理 P1 | — |\n', False),
            ('negation-grammar-irrelevant',
             '| 1 | DS | 无关 P1 | — |\n', False),
            ('negated-any-new-of', '| 1 | DS | 未发现任何新增的 P0/P1/P2 | — |\n', True),
            ('negated-new-severe-level',
             '| 1 | DS | 无新增严重级别 P0 | — |\n', True),
            # fix-r6: derived families — every closure verb keeps its failed,
            # in-progress, negated and intended forms (r5 reviews P0/P1).
            ('chinese-评审不通过', '| 1 | DS | P1：foo | 评审不通过 |\n', False),
            ('chinese-验收失败', '| 1 | DS | P1：foo | 验收失败 |\n', False),
            ('chinese-复核失败', '| 1 | DS | P1：foo | 复核失败 |\n', False),
            ('chinese-评审失败', '| 1 | DS | P1：foo | 评审失败 |\n', False),
            ('chinese-验证失败', '| 1 | DS | P1：foo | 验证失败 |\n', False),
            ('chinese-未保留', '| 1 | DS | P1：foo | 未保留 |\n', False),
            ('chinese-未拒绝', '| 1 | DS | P1：foo | 未拒绝 |\n', False),
            ('chinese-检查中', '| 1 | DS | P1：foo | 检查中 |\n', False),
            ('chinese-验证中', '| 1 | DS | P1：foo | 验证中 |\n', False),
            ('fixed-awaiting-verify', '| 1 | DS | P1：foo | 已修，待人工验证 |\n', False),
            ('fixed-awaiting-confirm', '| 1 | DS | P1：foo | 修复后待确认 |\n', False),
            ('chinese-暂时不修复', '| 1 | DS | P1：foo | 暂时不修复 |\n', False),
            ('chinese-暂且不修', '| 1 | DS | P1：foo | 暂且不修 |\n', False),
            ('chinese-需要修复', '| 1 | DS | P1：foo | 需要修复 |\n', False),
            ('chinese-需人工验收', '| 1 | DS | P1：foo | 需人工验收 |\n', False),
            ('chinese-下周修复', '| 1 | DS | P1：foo | 下周修复 |\n', False),
            ('chinese-稍后修复', '| 1 | DS | P1：foo | 稍后修复 |\n', False),
            ('chinese-将修复', '| 1 | DS | P1：foo | 将修复 |\n', False),
            ('english-will-be-fixed', '| 1 | DS | P1: foo | will be fixed |\n', False),
            ('english-to-be-fixed', '| 1 | DS | P1: foo | to be fixed |\n', False),
            ('english-not-planned-will-fix',
             '| 1 | DS | P1: foo | not planned for this release, will be fixed later |\n', False),
            ('english-long-gap-not',
             '| 1 | DS | P1: foo | not currently scheduled to be fixed |\n', False),
            # fix-r6: English negation binds to any verb in the clause,
            # across languages (reverse direction of the r5 pairs).
            ('cross-not-修', '| 1 | DS | P1: foo | not 修复 |\n', False),
            ('cross-never-合并', '| 1 | DS | P1: foo | never 合并 |\n', False),
            ("cross-hasnt-合入", "| 1 | DS | P1: foo | hasn't 合入 |\n", False),
            # fix-r6: guards — new markers must not reopen closed cells.
            ('guard-无需处理', '| 1 | DS | P1：foo | 无需处理 |\n', True),
            ('guard-不会修复-is-wontfix', '| 1 | DS | P1: foo | 不会修复 |\n', True),
            ('guard-已通过', '| 1 | DS | P1：foo | 已通过 |\n', True),
            ('guard-已迁移', '| 1 | DS | P1：foo | 已迁移至 #456 |\n', True),
            ('guard-已修复后续观察', '| 1 | DS | P1：foo | 已修复，后续观察 |\n', True),
            ('guard-不需要进一步处理',
             '| 1 | DS | P1：foo | 已修复，不需要进一步处理 |\n', True),
            # fix-r7 (r6 reviews, both families — structural round): the
            # in-progress form needs its right edge, so a verb+object noun
            # after 中 must not read in-progress (DS boundary#3).
            ('closed-升级中间件', '| 1 | DS | P1：foo | 已升级中间件到 2.1 |\n', True),
            ('closed-关闭中间层', '| 1 | DS | P1：foo | 已关闭中间层缓存 |\n', True),
            ('closed-保留中间态', '| 1 | DS | P1：foo | 已保留中间态 |\n', True),
            ('pending-升级中', '| 1 | DS | P1：foo | 升级中 |\n', False),
            # fix-r7: level negation is grammar, not a modifier whitelist —
            # a single-character modifier folds too (DS boundary#4).
            ('negated-single-新', '| 1 | DS | 未发现新 P1 | — |\n', True),
            ('negated-unseen-新', '| 1 | DS | 未见新 P1 | — |\n', True),
            ('negated-无新', '| 1 | DS | 无新 P1 | — |\n', True),
            # fix-r7: bare 需/会 intent markers carry morpheme guards, so
            # noun compounds must not reopen a closed row (MM boundary#3).
            ('marker-guard-需求方', '| 1 | DS | P1：foo | 已修复，需求方确认 |\n', True),
            ('marker-guard-需求已关闭', '| 1 | DS | P1：foo | 已修复，需求已关闭 |\n', True),
            ('marker-guard-会议已通过', '| 1 | DS | P1：foo | 已修复，会议已通过 |\n', True),
            ('marker-guard-机会再验证', '| 1 | DS | P1：foo | 已修复，有机会再验证 |\n', True),
            # fix-r7: the curly apostrophe folds before matching (MM#4) and
            # ASCII '.' splits clauses with digit protection (MM#5).
            ('english-curly-hasnt', "| 1 | DS | P1: foo | hasn’t been fixed |\n", False),
            ('english-curly-wontfix', "| 1 | DS | P1: foo | won’t fix |\n", True),
            ('english-period-wontfix',
             '| 1 | DS | P1: foo | Not reproducible. Closed as wontfix. |\n', True),
            ('english-period-merged',
             '| 1 | DS | P1: foo | Has not been fixed. Merged to main. |\n', False),
            ('english-period-digit-guard',
             '| 1 | DS | P1: foo | Fixed in v1.2. Merged to main. |\n', True),
            # fix-r7: the English verb source covers verification verbs, so a
            # negated one binds (DS boundary#2) and a bare one closes.
            ('english-not-verified', '| 1 | DS | P1: foo | fixed, but not verified |\n', False),
            ('english-verified-closes', '| 1 | DS | P1: foo | verified |\n', True),
            # fix-r7: right-extension guards keep 拒绝 / 保留意见 open (MM#2).
            ('refuse-拒绝修复', '| 1 | DS | P1：foo | 拒绝修复 |\n', False),
            ('refuse-已拒绝合入', '| 1 | DS | P1：foo | 已拒绝合入 |\n', False),
            ('refuse-保留意见', '| 1 | DS | P1：foo | 保留意见 |\n', False),
            # fix-r7: 待观察 joins its 保留观察/继续观察 family (MM boundary#8).
            ('pending-待观察', '| 1 | DS | P1：foo | 已修，待观察 |\n', False),
        ]
        # one runner for every per-round row list: write the record with the
        # row swapped in, run the pipeline, assert the verdict (r11 collapsed
        # the r7–r10 loops, which were four verbatim copies of these lines —
        # a fifth copy for the r11 list is how drift starts).
        def assert_rows(tag, cases):
            for name, row, expected in cases:
                with self.subTest(name=name):
                    record.write_text(
                        table.replace('| 1 | DS | P1：foo | 已修（c1） |\n', row)
                        + conclusion, encoding='utf-8')
                    with patch.object(G, 'WORKTREE', self.root):
                        self.assertIs(check_ruler.independent_review(), expected,
                                      '#2245-' + tag + ' row:' + name)
        # fix-r8 (closure-round DS review): one row case per r8 element, kept
        # in its own list so the failure markers carry the round they pin.
        row_cases_r8 = [
            # the in-progress right edge blocks only attested noun compounds
            # — an object after 的 keeps the row open (boundary#1).
            ('pending-修复中的问题已定位',
             '| 1 | DS | P1：foo | 修复中的问题已定位 |\n', False),
            ('pending-进行中的评审', '| 1 | DS | P1：foo | 进行中的评审 |\n', False),
            ('closed-已修进行中断分析',
             '| 1 | DS | P1：foo | 已修，进行中断分析 |\n', True),
            # 修改/维修/检修 join the single word source, and partial
            # completion never closes (boundary#2).
            ('pending-修改中', '| 1 | DS | P1：foo | 修改中 |\n', False),
            ('pending-维修中', '| 1 | DS | P1：foo | 维修中 |\n', False),
            ('pending-检修中', '| 1 | DS | P1：foo | 检修中 |\n', False),
            ('pending-修复了一半', '| 1 | DS | P1：foo | 修复了一半 |\n', False),
            ('pending-部分修复', '| 1 | DS | P1：foo | 部分修复 |\n', False),
            ('pending-阶段性修复', '| 1 | DS | P1：foo | 阶段性修复 |\n', False),
            ('closed-已修改', '| 1 | DS | P1：foo | 已修改 |\n', True),
            ('closed-已维修', '| 1 | DS | P1：foo | 已维修 |\n', True),
            ('closed-已检修', '| 1 | DS | P1：foo | 已检修 |\n', True),
            # out-of-table statement verbs and ASCII '.' keep a negated level
            # a real finding (boundary#3 / sibling#1).
            ('finding-尚未排除', '| 1 | DS | 尚未排除 P1 | — |\n', False),
            ('finding-未涉及', '| 1 | DS | 未涉及 P1 | — |\n', False),
            ('finding-未识别', '| 1 | DS | 未识别 P1 | — |\n', False),
            ('negated-句点后级别',
             '| 1 | DS | 未发现缓存泄漏.P1：锁无过期回收 | — |\n', False),
            # pin the negation-gap cap boundary (sibling#4): 8 characters
            # fold, 9 survive as a finding.
            ('negated-gap-8-fold', '| 1 | DS | 未发现本次改动中新引入P1 | — |\n', True),
            ('negated-gap-9-survive',
             '| 1 | DS | 未发现本次改动中新引入的P1 | — |\n', False),
            # every typographic apostrophe of the family folds on the
            # judgement copy (boundary#4) — the r6 MM#4 hole, other code points.
            ('english-curly-hasnt-u2018',
             "| 1 | DS | P1: foo | hasn‘t been fixed |\n", False),
            ('english-curly-hasnt-u02bc',
             '| 1 | DS | P1: foo | hasnʼt been fixed |\n', False),
            ('english-curly-hasnt-u2032',
             '| 1 | DS | P1: foo | hasn′t been fixed |\n', False),
            ('english-curly-hasnt-u201b',
             '| 1 | DS | P1: foo | hasn‛t been fixed |\n', False),
            # the English verb source covers disposal verbs, so an English
            # handling cell closes (sibling#5).
            ('english-accepted-closes', '| 1 | DS | P1: foo | accepted |\n', True),
            ('english-migrated-closes', '| 1 | DS | P1: foo | migrated to main |\n', True),
            ('english-passed-closes', '| 1 | DS | P1: foo | passed |\n', True),
            ('english-completed-closes', '| 1 | DS | P1: foo | completed |\n', True),
        ]
        assert_rows('r8', row_cases_r8)
        # fix-r9 (closure-round DS review, structural round): one row case per
        # r9 element — every DS r8 P0/P1 shape plus its pinned direction.
        row_cases_r9 = [
            # boundary#1 (P0): English partial modifiers and percentage
            # completion never close; guarded closure words read open.
            ('pending-completed-50pct', '| 1 | DS | P1: foo | completed 50% |\n', False),
            ('pending-partially-completed', '| 1 | DS | P1: foo | partially completed |\n', False),
            ('pending-almost-completed', '| 1 | DS | P1: foo | almost completed |\n', False),
            ('pending-migrated-partially', '| 1 | DS | P1: foo | migrated partially |\n', False),
            ('open-passed-to-next-round', '| 1 | DS | P1: foo | passed to next round |\n', False),
            ('open-accepted-risk', '| 1 | DS | P1: foo | accepted risk |\n', False),
            ('pending-fixed-70pct', '| 1 | DS | P1: foo | fixed 70% |\n', False),
            ('closed-verified-100pct', '| 1 | DS | P1: foo | verified 100% |\n', True),
            # boundary#2 / sibling#3 (P0/P1): an unknown verb between the
            # negation and the level keeps the level a real finding.
            ('finding-尚未定位', '| 1 | DS | 尚未定位 P1 根因 | — |\n', False),
            ('finding-未覆盖', '| 1 | DS | 未覆盖 P1 | — |\n', False),
            ('finding-未影响', '| 1 | DS | 未影响 P1 | — |\n', False),
            ('finding-尚未核对', '| 1 | DS | 尚未核对 P1 | — |\n', False),
            ('finding-尚未分析', '| 1 | DS | 尚未分析 P1 | — |\n', False),
            ('finding-尚未调查', '| 1 | DS | 尚未调查 P1 | — |\n', False),
            ('finding-尚未区分', '| 1 | DS | 尚未区分 P1 | — |\n', False),
            ('finding-尚未触发', '| 1 | DS | 尚未触发 P1 | — |\n', False),
            # boundary#6 / sibling#5 (P1): a digit-internal '.' stays inside
            # the strong negation gap (v1.2 folds; 泄漏.P1 never does). The
            # handling cell is the dash DS reported — a closed cell would
            # close the row either way and pin nothing.
            ('negated-v1.2-gap', '| 1 | DS | 未发现 v1.2 的 P1 | — |\n', True),
            ('negated-v1.2-tight', '| 1 | DS | 未发现v1.2引入P1 | — |\n', True),
            # boundary#4 / sibling#4 (P1): one mechanism for <X>中 — a noun
            # compound after 中 is closed for every preceding verb.
            ('closed-已测试中断场景', '| 1 | DS | P1：foo | 已测试中断场景 |\n', True),
            ('closed-已关闭中断', '| 1 | DS | P1：foo | 已关闭中断 |\n', True),
            ('closed-已测试中心化', '| 1 | DS | P1：foo | 已测试中心化改造 |\n', True),
            ('closed-已修进行中间件升级', '| 1 | DS | P1：foo | 已修复，进行中间件升级 |\n', True),
            ('closed-已修复中断的会话', '| 1 | DS | P1：foo | 已修复中断的会话 |\n', True),
            # sibling#1 (P1): <X>中 is structural — out-of-table verbs count.
            ('pending-回归中', '| 1 | DS | P1：foo | 已修复，回归中 |\n', False),
            ('pending-排查中', '| 1 | DS | P1：foo | 已修复，排查中 |\n', False),
            ('pending-定位中', '| 1 | DS | P1：foo | 已修复，定位中 |\n', False),
            # sibling#2 (P1): attested partial words beyond the r8 three.
            ('pending-局部修复', '| 1 | DS | P1：foo | 已修复，局部修复 |\n', False),
            ('pending-修复过半', '| 1 | DS | P1：foo | 修复过半 |\n', False),
            ('pending-未完全修复', '| 1 | DS | P1：foo | 未完全修复 |\n', False),
            # boundary#5 (P1): attested right extensions of the explicit
            # pending words are noun phrases inside a closed cell.
            ('closed-已修复不通过的用例', '| 1 | DS | P1：foo | 已修复不通过的用例 |\n', True),
            ('closed-已修复待办事项', '| 1 | DS | P1：foo | 已修复待办事项 |\n', True),
            ('closed-已处理保留意见变更', '| 1 | DS | P1：foo | 已处理保留意见变更 |\n', True),
            # boundary#3 / sibling#8 (P1/P2): the remaining attested
            # apostrophe code points fold (U+00B4 needs the pre-NFKC order).
            ('english-curly-hasnt-u02b9',
             '| 1 | DS | P1: foo | hasnʹt been fixed |\n', False),
            ('english-curly-hasnt-u00b4',
             '| 1 | DS | P1: foo | hasn´t been fixed |\n', False),
            ('english-curly-hasnt-u02bb',
             '| 1 | DS | P1: foo | hasnʻt been fixed |\n', False),
            ('english-curly-hasnt-u201a',
             '| 1 | DS | P1: foo | hasn‚t been fixed |\n', False),
            ('english-curly-hasnt-ua78c',
             '| 1 | DS | P1: foo | hasnꞌt been fixed |\n', False),
            # sibling#6 (P2): the English disposal verbs stay paired with
            # the Chinese side.
            ('english-upgraded-closes', '| 1 | DS | P1: foo | upgraded middleware to 2.1 |\n', True),
            ('english-modified-closes', '| 1 | DS | P1: foo | modified |\n', True),
            # anthropic boundary#6 (P0): apostrophe-free contraction stems
            # still negate — the closure verb must not rescue the row.
            ('english-hasnt-no-apostrophe', '| 1 | DS | P1: foo | hasnt been fixed |\n', False),
            ('english-isnt-merged', '| 1 | DS | P1: foo | isnt merged |\n', False),
            # anthropic boundary#9 (P2): 部分 inside the 全部分支 compound is
            # not a partial disposal — the row stays closed.
            ('closed-已合并全部分支', '| 1 | DS | P1：foo | 已合并全部分支 |\n', True),
        ]
        assert_rows('r9', row_cases_r9)
        # fix-r10 (closure-round DS + MM reviews, polarity round): one row
        # case per r10 element — the fail-closed <X>中 blacklist, the closure
        # guard shape families, the extended contraction stems, the
        # completion-context percentage, the residual quantity and the
        # weak-gap 有 fold.
        row_cases_r10 = [
            # A. <X>中 right edge is a fail-closed BLACKLIST (DS conc#1 /
            #    boundary#1 + MM boundary#1, P0 both lineages): every
            #    unattested successor reads pending…
            ('pending-修复中（待回归）',
             '| 1 | DS | P1：foo | 修复中（待回归） |\n', False),
            ('pending-回归中（等 CI）',
             '| 1 | DS | P1：foo | 已修复，回归中（等 CI） |\n', False),
            ('pending-修复中(见 #12)',
             '| 1 | DS | P1：foo | 修复中(见 #12) |\n', False),
            ('pending-修复中/待跟进',
             '| 1 | DS | P1：foo | 修复中/待跟进 |\n', False),
            ('pending-修复中【待】',
             '| 1 | DS | P1：foo | 修复中【待】 |\n', False),
            ('pending-修复中发现新问题',
             '| 1 | DS | P1：foo | 已修复，修复中发现新问题 |\n', False),
            ('pending-修复 中 (space before)',
             '| 1 | DS | P1：foo | 修复 中 |\n', False),
            ('pending-CI中 (latin before)',
             '| 1 | DS | P1：foo | CI中 |\n', False),
            # …while an attested compound successor stays closed, one row per
            # newly attested blacklist character (期/端/部/档/等/国/华). r11
            # (DS boundary#7, fixture realism): the r10 rows read 终端节点/
            # 内部审计/档期冲突/等级评定/国区网关/华夏分支 — the characters
            # appeared but never as 中X, so the blacklist was never exercised
            # (a vacuous control). Every row now carries the real 中X shape.
            ('closed-评审中期指标',
             '| 1 | DS | P1：foo | 评审中期指标 |\n', True),
            ('closed-已升级中端节点',
             '| 1 | DS | P1：foo | 已升级中端节点 |\n', True),
            ('closed-已完成中部审计',
             '| 1 | DS | P1：foo | 已完成中部审计 |\n', True),
            ('closed-已修复中档配置',
             '| 1 | DS | P1：foo | 已修复中档配置 |\n', True),
            ('closed-已修复中等严重',
             '| 1 | DS | P1：foo | 已修复中等严重 |\n', True),
            ('closed-已升级中国区网关',
             '| 1 | DS | P1：foo | 已升级中国区网关 |\n', True),
            ('closed-已合并中华分支',
             '| 1 | DS | P1：foo | 已合并中华分支 |\n', True),
            # known boundary, fail-closed direction (DS sibling#3 /
            # boundary#4, P2): word-final non-progressive 中 reads open.
            ('open-已修复，缺陷集中',
             '| 1 | DS | P1：foo | 已修复，缺陷集中 |\n', False),
            ('open-已修复其中的问题',
             '| 1 | DS | P1：foo | 已修复其中的问题 |\n', False),
            # B. closure guard shape families (DS sibling#1 / boundary#7,
            #    MM boundary#6): the guard blocks the whole extension family,
            #    the bare verb still closes.
            ('open-accepted the risk',
             '| 1 | DS | P1: foo | accepted the risk |\n', False),
            ('open-accepted as a risk',
             '| 1 | DS | P1: foo | accepted as a risk |\n', False),
            ('open-passed on to next round',
             '| 1 | DS | P1: foo | passed on to next round |\n', False),
            ('open-passed onto next round',
             '| 1 | DS | P1: foo | passed onto next round |\n', False),
            ('open-accepted, risk noted',
             '| 1 | DS | P1: foo | accepted, risk noted |\n', False),
            ('open-accepted for now',
             '| 1 | DS | P1: foo | accepted for now |\n', False),
            ('open-passed, to next round',
             '| 1 | DS | P1: foo | passed, to next round |\n', False),
            ('closed-bare accepted',
             '| 1 | DS | P1: foo | accepted |\n', True),
            ('closed-passed the review',
             '| 1 | DS | P1: foo | passed the review |\n', True),
            ('closed-accepted the change',
             '| 1 | DS | P1: foo | accepted the change |\n', True),
            # C. contraction family (DS boundary#2, MM sibling#2 / boundary#5):
            #    every attested apostrophe-free stem negates, cannot joins,
            #    wont keeps its right-extension distinction.
            ('open-havent been fixed',
             '| 1 | DS | P1: foo | havent been fixed |\n', False),
            ('open-havent merged',
             '| 1 | DS | P1: foo | havent merged |\n', False),
            ('open-dont be merged',
             '| 1 | DS | P1: foo | dont be merged |\n', False),
            ('open-cant be fixed',
             '| 1 | DS | P1: foo | cant be fixed |\n', False),
            ('open-mustnt be merged',
             '| 1 | DS | P1: foo | mustnt be merged |\n', False),
            ('open-cannot be fixed',
             '| 1 | DS | P1: foo | cannot be fixed |\n', False),
            ("open-won't be fixed",
             "| 1 | DS | P1: foo | won't be fixed |\n", False),
            ('open-wont be fixed',
             '| 1 | DS | P1: foo | wont be fixed |\n', False),
            ('open-hasnt been fixed',
             '| 1 | DS | P1: foo | hasnt been fixed |\n', False),
            # the curly apostrophe folds before the judgement copy, so the
            # pipeline sees the ASCII stem (r9-curly integration shape).
            ('open-codex 修（hasn’t been verified）',
             '| 1 | DS | P1：foo | codex 修（hasn’t been verified） |\n', False),
            ("closed-won't fix",
             "| 1 | DS | P1: foo | won't fix |\n", True),
            ('closed-wontfix',
             '| 1 | DS | P1: foo | wontfix |\n', True),
            # known boundary, fail-closed: only the apostrophe phrase and the
            # one-word wontfix close — bare "wont fix" reads open.
            ('open-wont fix (bare)',
             '| 1 | DS | P1: foo | wont fix |\n', False),
            # D. percentage completion context (DS conc#2 / boundary#3 /
            #    sibling#5, MM boundary#2 / #7): a completion verb with a
            #    sub-100 number reopens the row —
            ('open-completed 50%',
             '| 1 | DS | P1: foo | completed 50% |\n', False),
            ('open-completed 50 %',
             '| 1 | DS | P1: foo | completed 50 % |\n', False),
            ('open-completed 50 percent',
             '| 1 | DS | P1: foo | completed 50 percent |\n', False),
            ('open-fixed 99.5%',
             '| 1 | DS | P1: foo | fixed 99.5% |\n', False),
            ('open-修复了 70%',
             '| 1 | DS | P1：foo | 修复了 70% |\n', False),
            ('open-进度 80%',
             '| 1 | DS | P1：foo | 进度 80% |\n', False),
            ('open-已完成度 80%',
             '| 1 | DS | P1：foo | 已完成度 80% |\n', False),
            # — full values, rate metrics, coverage, leftovers and impact
            # notes never anchor (the r9 false-FAIL family).
            ('closed-verified 100%',
             '| 1 | DS | P1: foo | verified 100% |\n', True),
            ('closed-verified 100.0%',
             '| 1 | DS | P1: foo | verified 100.0% |\n', True),
            ('closed-verified 1000%',
             '| 1 | DS | P1: foo | verified 1000% |\n', True),
            ('closed-回归通过率 98%',
             '| 1 | DS | P1：foo | 已修复，回归通过率 98% |\n', True),
            ('closed-覆盖率 95%',
             '| 1 | DS | P1：foo | 已修复，覆盖率 95% |\n', True),
            ('closed-遗留 0%',
             '| 1 | DS | P1：foo | 已修复，遗留 0% |\n', True),
            ('closed-影响 30% 用户',
             '| 1 | DS | P1：foo | 已修复（影响 30% 用户） |\n', True),
            ('closed-pass rate 98%',
             '| 1 | DS | P1: foo | 已修复，pass rate 98% |\n', True),
            # a bare percentage with no completion verb never closes either.
            ('open-50% no verb',
             '| 1 | DS | P1: foo | 50% |\n', False),
            # E. residual quantity (MM boundary#3): the row stays open while
            #    a negated residual does not reopen it.
            ('open-还有 3 个',
             '| 1 | DS | P1：foo | 已修复不通过的用例，还有 3 个 |\n', False),
            ('open-剩余 2 项',
             '| 1 | DS | P1：foo | 已合并，剩余 2 项 |\n', False),
            ('open-仍有 1 项',
             '| 1 | DS | P1：foo | 已处置，仍有 1 项 |\n', False),
            ('closed-无剩余',
             '| 1 | DS | P1：foo | 已修，无剩余 |\n', True),
            ('closed-没有剩余',
             '| 1 | DS | P1：foo | 已修，没有剩余 |\n', True),
            # F. English partial family (MM sibling#3): the attested members
            #    never let a cell read closed.
            ('open-incomplete',
             '| 1 | DS | P1: foo | incomplete |\n', False),
            ('open-unfinished',
             '| 1 | DS | P1: foo | unfinished |\n', False),
            ('open-partly fixed',
             '| 1 | DS | P1: foo | partly fixed |\n', False),
            ('open-halfway done',
             '| 1 | DS | P1: foo | halfway done |\n', False),
            # G. downgraded / handled join the English closure side
            #    (DS boundary#6), paired with the Chinese 降级 row.
            ('closed-downgraded to P3',
             '| 1 | DS | P1: foo | downgraded to P3 |\n', True),
            ('closed-handled in c2',
             '| 1 | DS | P1: foo | handled in c2 |\n', True),
            ('closed-已降级（P3）',
             '| 1 | DS | P1：foo | 已降级（P3） |\n', True),
            # H. weak-gap 有 (MM boundary#4): 未有任何 P1 folds like
            #    无任何 P1 — the row reads clean.
            ('fold-未有任何 P1',
             '| 1 | DS | 未有任何 P1：foo | — |\n', True),
        ]
        # fix-r11 (closure-round DS + MM reviews, word-source round): one row
        # case per r11 element — the derived percentage verbs, the word-level
        # compound rules, the stacked accepted prefixes, the count-anchored
        # residual family, the scope-guarded partials, and the won't-only fix
        # exemption with its bare-infinitive binding. Every shape was first
        # confirmed live by the pre/post probes in
        # _tmp/20261110-issue-2245-closure/fix-r11/raw/ (relay-claims-verify),
        # then pinned here through the full pipeline.
        row_cases_r11 = [
            # A. percentage verbs derive from the closure word source, so the
            #    previously anchorless Chinese verbs read pending again.
            ('open-已测试 50%',
             '| 1 | DS | P1：foo | 已测试 50% |\n', False),
            ('open-已合并 50%',
             '| 1 | DS | P1：foo | 已合并 50% |\n', False),
            ('open-已验证 50%',
             '| 1 | DS | P1：foo | 已验证 50% |\n', False),
            ('open-已关闭 50%',
             '| 1 | DS | P1：foo | 已关闭 50% |\n', False),
            ('open-已降级 50%',
             '| 1 | DS | P1：foo | 已降级 50% |\n', False),
            ('open-已处理 70%',
             '| 1 | DS | P1：foo | 已处理 70% |\n', False),
            ('open-已解决 30%',
             '| 1 | DS | P1：foo | 已解决 30% |\n', False),
            ('open-completed 50% (r11 walk)',
             '| 1 | DS | P1: foo | completed 50% |\n', False),
            ('open-修复了 70% (r11 walk)',
             '| 1 | DS | P1：foo | 修复了 70% |\n', False),
            # B. compound rules are word-level: a guarded character reads
            #    pending outside its attested compound, the new noun seconds
            #    (台/控/南/西) read closed.
            ('open-已修复中等待回归',
             '| 1 | DS | P1：foo | 已修复中等待回归 |\n', False),
            ('open-修复中等待 CI',
             '| 1 | DS | P1：foo | 修复中等待 CI |\n', False),
            ('open-已修复，回归中等待 CI',
             '| 1 | DS | P1：foo | 已修复，回归中等待 CI |\n', False),
            ('open-已修复，修复中间发现新问题',
             '| 1 | DS | P1：foo | 已修复，修复中间发现新问题 |\n', False),
            ('open-修复中期评估',
             '| 1 | DS | P1：foo | 修复中期评估 |\n', False),
            ('closed-已修复中台服务',
             '| 1 | DS | P1：foo | 已修复中台服务 |\n', True),
            ('closed-已修复中控服务',
             '| 1 | DS | P1：foo | 已修复中控服务 |\n', True),
            ('closed-已修复中南区网关',
             '| 1 | DS | P1：foo | 已修复中南区网关 |\n', True),
            ('closed-已修复中西部节点',
             '| 1 | DS | P1：foo | 已修复中西部节点 |\n', True),
            ('closed-已测试中断场景 (r11 walk)',
             '| 1 | DS | P1：foo | 已测试中断场景 |\n', True),
            ('closed-已升级中间件 (r11 walk)',
             '| 1 | DS | P1：foo | 已升级中间件 |\n', True),
            ('closed-已完成中心化改造 (r11 walk)',
             '| 1 | DS | P1：foo | 已完成中心化改造 |\n', True),
            ('closed-评审中期指标 (r11 walk)',
             '| 1 | DS | P1：foo | 评审中期指标 |\n', True),
            ('open-修复中（待回归） (r11 walk)',
             '| 1 | DS | P1：foo | 修复中（待回归） |\n', False),
            ('open-已修复，修复中发现新问题 (r11 walk)',
             '| 1 | DS | P1：foo | 已修复，修复中发现新问题 |\n', False),
            # C. accepted blocks a stacked prefix family (preposition*, then
            #    any one determiner) before risk(s).
            ('open-accepted a risk',
             '| 1 | DS | P1: foo | accepted a risk |\n', False),
            ('open-accepted this risk',
             '| 1 | DS | P1: foo | accepted this risk |\n', False),
            ('open-accepted with a risk',
             '| 1 | DS | P1: foo | accepted with a risk |\n', False),
            ('open-accepted all risks',
             '| 1 | DS | P1: foo | accepted all risks |\n', False),
            ('open-accepted with the risk',
             '| 1 | DS | P1: foo | accepted with the risk |\n', False),
            # round-1 realdata catch: a preposition with NO determiner still
            # blocks (the draft stacked form required one — the r9 poison
            # 'accepted as risk' re-passed as closed and was caught by the
            # fix-r11 realdata recompute).
            ('open-accepted as risk (no determiner)',
             '| 1 | DS | P1: foo | accepted as risk |\n', False),
            ('open-accepted with risk (no determiner)',
             '| 1 | DS | P1: foo | accepted with risk |\n', False),
            ('open-accepted as this risk',
             '| 1 | DS | P1: foo | accepted as this risk |\n', False),
            ('open-accepted in the risk',
             '| 1 | DS | P1: foo | accepted in the risk |\n', False),
            ('open-accepted the risk (r11 walk)',
             '| 1 | DS | P1: foo | accepted the risk |\n', False),
            ('closed-bare accepted (r11 walk)',
             '| 1 | DS | P1: foo | accepted |\n', True),
            # D. the percentage gap allows digits and brackets, still
            #    refusing clause punctuation, %, / and a second rate/影响.
            ('open-已修复 3 个用例 50%',
             '| 1 | DS | P1：foo | 已修复 3 个用例 50% |\n', False),
            ('open-completed 3 of 5 (60%)',
             '| 1 | DS | P1: foo | completed 3 of 5 (60%) |\n', False),
            ('open-已完成（80%）',
             '| 1 | DS | P1：foo | 已完成（80%） |\n', False),
            ('open-fixed (50%)',
             '| 1 | DS | P1: foo | fixed (50%) |\n', False),
            ('open-已修复，遗留 20% 未覆盖用例',
             '| 1 | DS | P1：foo | 已修复，遗留 20% 未覆盖用例 |\n', False),
            ('closed-已修复（影响 30% 用户） (r11 walk)',
             '| 1 | DS | P1：foo | 已修复（影响 30% 用户） |\n', True),
            ('closed-已修复，回归通过率 98% (r11 walk)',
             '| 1 | DS | P1：foo | 已修复，回归通过率 98% |\n', True),
            ('closed-已修复，遗留 0% (r11 walk)',
             '| 1 | DS | P1：foo | 已修复，遗留 0% |\n', True),
            # E. partial words carry scope guards.
            ('closed-已修复，影响部分用户',
             '| 1 | DS | P1：foo | 已修复，影响部分用户 |\n', True),
            ('closed-已修复（almost no impact）',
             '| 1 | DS | P1: foo | 已修复（almost no impact） |\n', True),
            ('closed-已修复，局部地区',
             '| 1 | DS | P1：foo | 已修复，局部地区 |\n', True),
            ('open-部分修复 (r11 walk)',
             '| 1 | DS | P1：foo | 部分修复 |\n', False),
            ('open-partly fixed (r11 walk)',
             '| 1 | DS | P1: foo | partly fixed |\n', False),
            # F. the residual family is count-anchored and bilingual.
            ('open-已修复，遗留 2 项',
             '| 1 | DS | P1：foo | 已修复，遗留 2 项 |\n', False),
            ('open-已修复，尚余 2 个',
             '| 1 | DS | P1：foo | 已修复，尚余 2 个 |\n', False),
            ('open-fixed, 3 remaining',
             '| 1 | DS | P1: foo | fixed, 3 remaining |\n', False),
            ('closed-已修，无任何剩余',
             '| 1 | DS | P1：foo | 已修，无任何剩余 |\n', True),
            ('closed-已修复，剩余风险已接受',
             '| 1 | DS | P1：foo | 已修复，剩余风险已接受 |\n', True),
            ('closed-已修，还有 0 个',
             '| 1 | DS | P1：foo | 已修，还有 0 个 |\n', True),
            ('closed-已合并，还剩 0 项',
             '| 1 | DS | P1：foo | 已合并，还剩 0 项 |\n', True),
            ('closed-已修复，另有说明',
             '| 1 | DS | P1：foo | 已修复，另有说明 |\n', True),
            # r12 (DS sibling#5 P1): the r11 pin was itself the regression —
            # 另有风险 names an additional RISK, so the noun anchor reopens
            # it; the flipped expectation registers the direction.
            ('open-已修复，另有风险 (r12 flip of the r11 pin)',
             '| 1 | DS | P1：foo | 已修复，另有风险 |\n', False),
            ('closed-已修复，half-open ticket',
             '| 1 | DS | P1: foo | 已修复，half-open ticket |\n', True),
            ('open-已修复不通过的用例，还有 3 个 (r11 walk)',
             '| 1 | DS | P1：foo | 已修复不通过的用例，还有 3 个 |\n', False),
            # G. only won't keeps the fix exemption; every other contraction
            #    negates again, with the bare infinitive binding.
            ("open-didn't fix the leak, verified",
             "| 1 | DS | P1: foo | didn't fix the leak, verified |\n", False),
            ("open-can't fix, merged",
             "| 1 | DS | P1: foo | can't fix, merged |\n", False),
            ("closed-won't fix (r11 walk)",
             "| 1 | DS | P1: foo | won't fix |\n", True),
            ("open-won't be fixed (r11 walk)",
             "| 1 | DS | P1: foo | won't be fixed |\n", False),
            ('open-hasnt been fixed (r11 walk)',
             '| 1 | DS | P1: foo | hasnt been fixed |\n', False),
            # H. an embedded English verb no longer anchors across the
            #    clause punctuation.
            ('closed-fixed; attested 50%',
             '| 1 | DS | P1: foo | fixed; attested 50% |\n', True),
            # I. up to two blanks before 中 are tolerated.
            ('open-修复  中 (double space)',
             '| 1 | DS | P1：foo | 修复  中 |\n', False),
            # J. any run of blanks before the percent sign is tolerated.
            ('open-completed 50  percent (double space)',
             '| 1 | DS | P1: foo | completed 50  percent |\n', False),
        ]
        # fix-r12 (DS r11 review, head 5f037b6e1): one row per P0/P1 element
        # and each cheap P2 folded into the same line — the MM r11 review
        # died on a 429 Token Plan exhaustion (rev-r11-mm-429), so the r12
        # round is re-reviewed from the same base with both lineages.
        row_cases_r12 = [
            # A. residual nouns ride the quantifier family (DS b#1 P0,
            #    s#5 P1 — 另有风险 flips, the r11 pin was the regression).
            ('open-已修复，还有问题',
             '| 1 | DS | P1：foo | 已修复，还有问题 |\n', False),
            ('open-已修复，仍有风险',
             '| 1 | DS | P1：foo | 已修复，仍有风险 |\n', False),
            ('open-已修复，还有隐患',
             '| 1 | DS | P1：foo | 已修复，还有隐患 |\n', False),
            ('closed-已修复，另有说明 (r12 walk)',
             '| 1 | DS | P1：foo | 已修复，另有说明 |\n', True),
            ('closed-已修复，剩余风险已接受 (r12 walk)',
             '| 1 | DS | P1：foo | 已修复，剩余风险已接受 |\n', True),
            # B. the count-word class covers the numeral family (DS b#2 P0,
            #    s#2 P1).
            ('open-已修复，还有两处未覆盖',
             '| 1 | DS | P1：foo | 已修复，还有两处未覆盖 |\n', False),
            ('open-已修复，还有两个问题',
             '| 1 | DS | P1：foo | 已修复，还有两个问题 |\n', False),
            ('open-已修复，还有多个用例',
             '| 1 | DS | P1：foo | 已修复，还有多个用例 |\n', False),
            ('open-已修复，还有数处',
             '| 1 | DS | P1：foo | 已修复，还有数处 |\n', False),
            ('closed-已修，还有 0 个 (r12 walk)',
             '| 1 | DS | P1：foo | 已修，还有 0 个 |\n', True),
            # C. 遗留 negation covers the multi-char form (DS s#4 P1, and
            #    the left guard finally has a row that exercises it, s#9).
            ('closed-已修复，没有遗留问题',
             '| 1 | DS | P1：foo | 已修复，没有遗留问题 |\n', True),
            ('closed-已修，无遗留 (r12 walk)',
             '| 1 | DS | P1：foo | 已修，无遗留 |\n', True),
            ('open-已修复，遗留问题 (r12 walk)',
             '| 1 | DS | P1：foo | 已修复，遗留问题 |\n', False),
            # D. English zero excluded, bounded noun gap, prepend form
            #    (DS b#5 P1 / s#6 P1 / b#6 P2).
            ('closed-fixed, 0 remaining',
             '| 1 | DS | P1: foo | 已修，fixed, 0 remaining |\n', True),
            ('closed-still 0 open',
             '| 1 | DS | P1: foo | 已修，still 0 open |\n', True),
            ('open-fixed, 3 issues remaining',
             '| 1 | DS | P1: foo | fixed, 3 issues remaining |\n', False),
            ('open-fixed, 3 findings left',
             '| 1 | DS | P1: foo | fixed, 3 findings left |\n', False),
            ('open-fixed, remaining 3',
             '| 1 | DS | P1: foo | fixed, remaining 3 |\n', False),
            # E. half blocks only the attested closed compound (DS b#3 P0,
            #    s#1 P1, simplify#1 P2).
            ('open-half-way done',
             '| 1 | DS | P1: foo | half-way done |\n', False),
            ('open-half-fixed',
             '| 1 | DS | P1: foo | half-fixed |\n', False),
            ('closed-已修复，half-open ticket (r12 walk)',
             '| 1 | DS | P1: foo | 已修复，half-open ticket |\n', True),
            # F. accepted blocks the quantifier determiners (DS b#4 P1).
            ('open-accepted every risk',
             '| 1 | DS | P1: foo | accepted every risk |\n', False),
            ('open-accepted each risk',
             '| 1 | DS | P1: foo | accepted each risk |\n', False),
            ('open-accepted several risks',
             '| 1 | DS | P1: foo | accepted several risks |\n', False),
            ('open-accepted 3 risks',
             '| 1 | DS | P1: foo | accepted 3 risks |\n', False),
            ('closed-bare accepted (r12 walk)',
             '| 1 | DS | P1: foo | accepted |\n', True),
            ('closed-accepted the change (r12 walk)',
             '| 1 | DS | P1: foo | accepted the change |\n', True),
            # G. 断/部/控 carry continuation guards (DS s#3 P1, folds b#9).
            ('open-修复中断，等待回归',
             '| 1 | DS | P1：foo | 修复中断，等待回归 |\n', False),
            ('open-修复中部署新版本',
             '| 1 | DS | P1：foo | 修复中部署新版本 |\n', False),
            ('open-修复中控制变量',
             '| 1 | DS | P1：foo | 修复中控制变量 |\n', False),
            ('closed-已测试中断场景 (r12 walk)',
             '| 1 | DS | P1：foo | 已测试中断场景 |\n', True),
            ('closed-已关闭中断 (r12 walk)',
             '| 1 | DS | P1：foo | 已关闭中断 |\n', True),
            ('closed-已完成中部审计 (r12 walk)',
             '| 1 | DS | P1：foo | 已完成中部审计 |\n', True),
            ('closed-已修复中控服务 (r12 walk)',
             '| 1 | DS | P1：foo | 已修复中控服务 |\n', True),
            # H. nearly mirrors the almost no-exclusion (DS s#7 P1).
            ('closed-已修复（nearly no impact）',
             '| 1 | DS | P1: foo | 已修复（nearly no impact） |\n', True),
            ('open-nearly finished',
             '| 1 | DS | P1: foo | nearly finished |\n', False),
            # I. the y→i stem family binds the bare infinitive (DS s#8 P1,
            #    folds b#7 P2).
            ("open-didn't verify the fix, merged upstream",
             "| 1 | DS | P1: foo | didn't verify the fix, merged upstream |\n", False),
            ("open-didn't modify the API, merged",
             "| 1 | DS | P1: foo | didn't modify the API, merged |\n", False),
            ("open-can't fix, merged (r12 walk)",
             "| 1 | DS | P1: foo | can't fix, merged |\n", False),
            # J. the blank run before 中 is unbounded (DS b#8 P2).
            ('open-修复   中 (triple space)',
             '| 1 | DS | P1：foo | 修复   中 |\n', False),
            # K. the pct gap exclusion covers the inflected impact family
            #    (DS b#10 P2, false FAIL).
            ('closed-fixed impacted 30% of users',
             '| 1 | DS | P1: foo | fixed impacted 30% of users |\n', True),
        ]
        # fix-r13 (DS r12 review, head f4dfd670d): the same per-element rows
        # for the shape-level rewrite — accepted window, unitized pct gap,
        # quantifier window, the 已+verb disposition filter, the partial
        # family, the English remain inflection, the attested 端/层 lookaheads
        # and the frozen residual nouns.
        row_cases_r13 = [
            # A. accepted blocks a bounded token window onto risks (DS b#1
            #    P0) — every shape the r12 determiner enumeration missed.
            ('open-accepted the remaining risks',
             '| 1 | DS | P1: foo | accepted the remaining risks |\n', False),
            ('open-accepted all of the risks',
             '| 1 | DS | P1: foo | accepted all of the risks |\n', False),
            ('open-accepted three risks',
             '| 1 | DS | P1: foo | accepted three risks |\n', False),
            ('open-accepted most risks',
             '| 1 | DS | P1: foo | accepted most risks |\n', False),
            ('open-accepted some risks',
             '| 1 | DS | P1: foo | accepted some risks |\n', False),
            ('open-accepted the two risks',
             '| 1 | DS | P1: foo | accepted the two risks |\n', False),
            ('open-accepted as a residual risk',
             '| 1 | DS | P1: foo | accepted as a residual risk |\n', False),
            ('open-accepted; risk noted',
             '| 1 | DS | P1: foo | accepted; risk noted |\n', False),
            # B. the pct gap counts a digit run or a letter run as ONE unit
            #    (DS b#2 P0) — the 12-unit cap decouples from digit count.
            ('open-completed 300 of 500 (60%)',
             '| 1 | DS | P1: foo | completed 300 of 500 (60%) |\n', False),
            ('open-fixed after retesting 50%',
             '| 1 | DS | P1: foo | fixed after retesting 50% |\n', False),
            ('open-completed 3000 of 50000 (60%)',
             '| 1 | DS | P1: foo | completed 3000 of 50000 (60%) |\n', False),
            # C. the residual quantifier anchors a bounded 4-char window
            #    (DS b#3 P0) — the count-word class gains its quantifier tail.
            ('open-已修复，还有若干处未覆盖',
             '| 1 | DS | P1：foo | 已修复，还有若干处未覆盖 |\n', False),
            ('open-已修复，还有少量问题',
             '| 1 | DS | P1：foo | 已修复，还有少量问题 |\n', False),
            ('open-已修复，还有较多未覆盖用例',
             '| 1 | DS | P1：foo | 已修复，还有较多未覆盖用例 |\n', False),
            ('open-已修复，还有不少问题',
             '| 1 | DS | P1：foo | 已修复，还有不少问题 |\n', False),
            ('open-已修复，还有：3 个',
             '| 1 | DS | P1：foo | 已修复，还有：3 个 |\n', False),
            ('closed-已修复，还有别的说明 (r13 boundary)',
             '| 1 | DS | P1：foo | 已修复，还有别的说明 |\n', True),
            # D. 已 + closure verb + guarded second is a disposition OBJECT,
            #    not a progress marker (DS b#4 P1, false FAIL); unknown
            #    seconds keep the fail-closed pend of the r11 pin.
            ('closed-已修复中断，回归通过',
             '| 1 | DS | P1：foo | 已修复中断，回归通过 |\n', True),
            ('closed-已关闭中断，后续观察',
             '| 1 | DS | P1：foo | 已关闭中断，后续观察 |\n', True),
            ('open-已修复，回归中 (r13 boundary)',
             '| 1 | DS | P1：foo | 已修复，回归中 |\n', False),
            # E. the partial left guard rides the whole family (DS s#2 P1).
            ('closed-已修复，影响局部用户',
             '| 1 | DS | P1：foo | 已修复，影响局部用户 |\n', True),
            ('closed-已修复，影响过半用户',
             '| 1 | DS | P1：foo | 已修复，影响过半用户 |\n', True),
            ('closed-已修复，影响一半用户',
             '| 1 | DS | P1：foo | 已修复，影响一半用户 |\n', True),
            # F. the English remain inflection joins the residual family
            #    (DS s#3 P1); zero stays closed, a count pends.
            ('open-fixed, 3 issues remain',
             '| 1 | DS | P1: foo | fixed, 3 issues remain |\n', False),
            ('open-fixed, still issues remain',
             '| 1 | DS | P1: foo | fixed, still issues remain |\n', False),
            ('open-still 3 issues remain',
             '| 1 | DS | P1: foo | still 3 issues remain |\n', False),
            ('closed-已修，still 0 issues remain (r13 zero)',
             '| 1 | DS | P1: foo | 已修，still 0 issues remain |\n', True),
            # G. 端/层 grow attested lookahead shapes (DS s#6 P2) — bare
            #    characters close nothing, only the attested nouns do.
            ('open-修复中端详日志',
             '| 1 | DS | P1：foo | 修复中端详日志 |\n', False),
            ('open-修复中端掉旧服务',
             '| 1 | DS | P1：foo | 修复中端掉旧服务 |\n', False),
            ('open-修复中层叠缓存',
             '| 1 | DS | P1：foo | 修复中层叠缓存 |\n', False),
            ('closed-已升级中层的节点 (r13 genitive)',
             '| 1 | DS | P1：foo | 已升级中层的节点 |\n', True),
            # H. the frozen residual nouns walk live (DS s#5/b#5 P2,
            #    self-source blindness).
            ('open-已修复，还有缺陷',
             '| 1 | DS | P1：foo | 已修复，还有缺陷 |\n', False),
            ('open-已修复，遗留隐患',
             '| 1 | DS | P1：foo | 已修复，遗留隐患 |\n', False),
        ]
        # fix-r14 (DS+MM r13 review, head bdbc50148): the structural round —
        # every bounded window constant became clause-scope, the pct units
        # are possessive and mutually exclusive (the backtracking P0 both
        # lineages found independently), the disposal verb check derives
        # from the gate-verb table, and 待 gains the await construction.
        row_cases_r14 = [
            # A. clause-scope replaces every length constant — a modifier run
            #    one past any cap, a risk noun past the 4th word unit, one
            #    glued to punctuation, a full-width separator and a 13-unit
            #    pct tail all reopen (DS b#1/b#2/b#3, MM b#1/b#2/b#4, DS s#2).
            ('open-已修复，还有大量未覆盖问题',
             '| 1 | DS | P1：foo | 已修复，还有大量未覆盖问题 |\n', False),
            ('open-已修复，还有极个别遗留的隐患',
             '| 1 | DS | P1：foo | 已修复，还有极个别遗留的隐患 |\n', False),
            ('open-已修复，还有少量未覆盖用例',
             '| 1 | DS | P1：foo | 已修复，还有少量未覆盖用例 |\n', False),
            ('open-已修复，还有未覆盖用例',
             '| 1 | DS | P1：foo | 已修复，还有未覆盖用例 |\n', False),
            ('open-accepted all of the remaining open risks',
             '| 1 | DS | P1: foo | accepted all of the remaining open risks |\n', False),
            ('open-accepted with the caveat that the risk remains',
             '| 1 | DS | P1: foo | accepted with the caveat that the risk remains |\n', False),
            ('open-accepted in spite of the remaining risks',
             '| 1 | DS | P1: foo | accepted in spite of the remaining risks |\n', False),
            ('open-accepted (risk noted)',
             '| 1 | DS | P1: foo | accepted (risk noted) |\n', False),
            ('open-accepted/risk noted',
             '| 1 | DS | P1: foo | accepted/risk noted |\n', False),
            ('open-accepted，risk noted (full-width)',
             '| 1 | DS | P1: foo | accepted，risk noted |\n', False),
            ('open-passed; to next round',
             '| 1 | DS | P1: foo | passed; to next round |\n', False),
            ('open-fixed, still a few issues remain',
             '| 1 | DS | P1: foo | fixed, still a few issues remain |\n', False),
            ('open-已完成 12 项中的 10 项的 50%',
             '| 1 | DS | P1：foo | 已完成 12 项中的 10 项的 50% |\n', False),
            ('open-fixed after retesting the sharded cluster 50%',
             '| 1 | DS | P1: foo | fixed after retesting the sharded cluster '
             'in the eu west region today 50% |\n', False),
            # B. the 遗留 branch joins the quantifier window (DS s#1 P1) —
            #    a modifier before the noun or the ：form reopens.
            ('open-已修复，遗留少量问题',
             '| 1 | DS | P1：foo | 已修复，遗留少量问题 |\n', False),
            ('open-已修复，遗留较多未覆盖用例',
             '| 1 | DS | P1：foo | 已修复，遗留较多未覆盖用例 |\n', False),
            ('open-已修复，遗留：3 个',
             '| 1 | DS | P1：foo | 已修复，遗留：3 个 |\n', False),
            # C. the await construction pends without a gate verb (DS b#4 P0)
            #    — 等待回归 / 待回归 name an outstanding action of their own.
            ('open-已修复中断，等待回归',
             '| 1 | DS | P1：foo | 已修复中断，等待回归 |\n', False),
            ('open-已修复，待回归',
             '| 1 | DS | P1：foo | 已修复，待回归 |\n', False),
            ('open-已修复，期待人工验收 (r14 no-left-guard)',
             '| 1 | DS | P1：foo | 已修复，期待人工验收 |\n', False),
            # D. the disposal clause-break family takes brackets / period /
            #    slash (DS b#5 P1, MM b#3 P1) — same shape, same verdict.
            ('closed-已修复中断（回归通过）',
             '| 1 | DS | P1：foo | 已修复中断（回归通过） |\n', True),
            ('closed-已修复中断/回归通过',
             '| 1 | DS | P1：foo | 已修复中断/回归通过 |\n', True),
            ('closed-已修复中断.',
             '| 1 | DS | P1：foo | 已修复中断. |\n', True),
            # E. the disposal verb derives from the gate-verb table (DS b#7 /
            #    MM s#5 / DS s#3 P2) — single-char verbs, 了 and an adverbial
            #    between 已 and the verb are all dispositions now.
            ('closed-已修中断，回归通过',
             '| 1 | DS | P1：foo | 已修中断，回归通过 |\n', True),
            ('closed-已修复了中断，回归通过',
             '| 1 | DS | P1：foo | 已修复了中断，回归通过 |\n', True),
            ('closed-已成功修复中断，回归通过',
             '| 1 | DS | P1：foo | 已成功修复中断，回归通过 |\n', True),
            # F. the partial family covers all six members (DS b#6 / MM s#3
            #    P2) and the English half gains the impact/affect left guard
            #    (DS s#4② P2) — scope prose closes on both sides.
            ('closed-已修复，影响阶段性目标',
             '| 1 | DS | P1：foo | 已修复，影响阶段性目标 |\n', True),
            ('closed-已修复，影响未完全覆盖的用例',
             '| 1 | DS | P1：foo | 已修复，影响未完全覆盖的用例 |\n', True),
            ('closed-fixed; impact on half of the users',
             '| 1 | DS | P1: foo | fixed; impact on half of the users |\n', True),
            # G. bare single characters guard on their attested continuations
            #    (DS s#5 P2) — 修复中心跳异常 / 修复中台账异常 reopen.
            ('open-修复中心跳异常',
             '| 1 | DS | P1：foo | 修复中心跳异常 |\n', False),
            ('open-修复中台账异常',
             '| 1 | DS | P1：foo | 修复中台账异常 |\n', False),
        ]
        # fix-r15 (DS r14 review, head 87871a970): the P0 left bound on the
        # disposal run, the separator-family extensions, the English await
        # and residual-gap fixes, and the Unicode-Nd single parse.
        row_cases_r15 = [
            # A. the disposal prefix gains its LEFT bound (DS boundary#1 P0,
            #    measured live): a gate verb INSIDE the run between 已 and the
            #    final verb is a new clause's work, not the disposal object.
            ('open-已修复A但复检中断，需要继续',
             '| 1 | DS | P1：foo | 已修复A但复检中断，需要继续 |\n', False),
            # B. the separator families take the square-bracket / hyphen /
            #    sentence-punctuation members (DS boundary#2 P1 / sibling#2
            #    P1) — the same verdict as the round-bracket twins.
            ('closed-已修复中断[回归通过]',
             '| 1 | DS | P1：foo | 已修复中断[回归通过] |\n', True),
            ('closed-已修复中断【回归通过】',
             '| 1 | DS | P1：foo | 已修复中断【回归通过】 |\n', True),
            ('open-accepted.risk noted',
             '| 1 | DS | P1: foo | accepted.risk noted |\n', False),
            ('open-accepted the remaining-risk',
             '| 1 | DS | P1: foo | accepted the remaining-risk |\n', False),
            ('open-accepted-risk',
             '| 1 | DS | P1: foo | accepted-risk |\n', False),
            # C. awaiting / waiting for join the explicit pending family (DS
            #    sibling#1 P1) — the English mirror of 等待回归.
            ('open-fixed, awaiting retest',
             '| 1 | DS | P1: foo | fixed, awaiting retest |\n', False),
            ('open-fixed, waiting for retest',
             '| 1 | DS | P1: foo | fixed, waiting for retest |\n', False),
            # D. the impact/affect left guards cover every non-exclusion
            #    member and the participle inflections (DS sibling#3 P1 /
            #    boundary#4 P2) — scope prose closes on both sides.
            ('closed-fixed; impact on partial rollout',
             '| 1 | DS | P1: foo | fixed; impact on partial rollout |\n', True),
            ('closed-fixed; the change affected half of the users',
             '| 1 | DS | P1: foo | fixed; the change affected half of the '
             'users |\n', True),
            ('closed-fixed, impacting partially rolled out clusters',
             '| 1 | DS | P1: foo | fixed, impacting partially rolled out '
             'clusters |\n', True),
            # E. the await construction takes Latin and digits after an
            #    optional space (DS sibling#4 P1).
            ('open-已修复，等待 CI',
             '| 1 | DS | P1：foo | 已修复，等待 CI |\n', False),
            ('open-已修复，等待 3 天',
             '| 1 | DS | P1：foo | 已修复，等待 3 天 |\n', False),
            # F. the English digit→noun gap loses its {0,3} word cap (DS
            #    sibling#5 P1 — the underlying constant the reviewer's own
            #    example missed; the 4-word gap shape reopens).
            ('open-fixed, 3 of these lingering critical issues remaining',
             '| 1 | DS | P1: foo | fixed, 3 of these lingering critical '
             'issues remaining |\n', False),
            # G. 'passed' gains the for-now deferral branch (MM r13
            #    boundary#6 P2, DS r14 boundary#6) — same shape as accepted.
            ('open-passed for now',
             '| 1 | DS | P1: foo | passed for now |\n', False),
            ('open-accepted for now (re-attest)',
             '| 1 | DS | P1: foo | accepted for now |\n', False),
            # H. the domain residual nouns walk with behavior controls (DS
            #    sibling#6 P2) — 场景 / 脚本 join the 用例 shapes of r14.
            ('open-已修复，还有未覆盖场景',
             '| 1 | DS | P1：foo | 已修复，还有未覆盖场景 |\n', False),
            ('open-已修复，遗留脚本',
             '| 1 | DS | P1：foo | 已修复，遗留脚本 |\n', False),
            # I. Unicode Nd digits stay single-branch (DS boundary#3 P2,
            #    measured): the Arabic-Indic percentage anchors like the
            #    ASCII twin — partial value reopens, full value closes.
            ('open-已完成 ٣٠% (unicode nd digits)',
             '| 1 | DS | P1：foo | 已完成 ٣٠% |\n', False),
            ('closed-已完成 ١٠٠% (unicode nd digits)',
             '| 1 | DS | P1：foo | 已完成 ١٠٠% |\n', True),
        ]
        assert_rows('r15', row_cases_r15)
        assert_rows('r14', row_cases_r14)
        assert_rows('r13', row_cases_r13)
        assert_rows('r12', row_cases_r12)
        assert_rows('r11', row_cases_r11)
        assert_rows('r10', row_cases_r10)
        assert_rows('r7', row_cases)
        # fix-r7 invariant 1 (both families' P0): dual-column records judge
        # every 处理/处置 cell on its own — a closed sibling never rescues a
        # dash or a pending one, and pending in any single cell fails the row.
        dual_table = ('# 记录\n\n| 轮 | 审查方 | 结论 | 处理 | 处置 |\n|---|---|---|---|---|\n')
        dual_cases = [
            ('dual-both-closed', '已修（c1）', '已修', True),
            ('dual-chuzhi-dash', '已修（c1）', '—', False),
            ('dual-chuli-dash', '—', '已修', False),
            ('dual-pending-second', '已修复', '暂缓', False),
            ('dual-pending-first', '暂缓', '已修复', False),
            ('dual-both-dash', '—', '—', False),
        ]
        for name, h1, h2, expected in dual_cases:
            with self.subTest(name=name):
                record.write_text(dual_table + f'| 1 | DS | P1：foo | {h1} | {h2} |\n'
                                  + conclusion, encoding='utf-8')
                with patch.object(G, 'WORKTREE', self.root):
                    self.assertIs(check_ruler.independent_review(), expected,
                                  '#2245-r7 dual:' + name)
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
            # fix-r4: ~~~ fences hide content exactly like ``` fences.
            ('tilde-fence-hides-conclusion',
             table + '~~~\n' + conclusion.strip() + '\n~~~\n', False),
            ('tilde-example-poison-ignored', table + conclusion
             + '~~~\n| 结论 | 处理 |\n|---|---|\n| P1：x | — |\n~~~\n', True),
            ('tilde-unclosed-fence-hides-poison',
             table + conclusion + '~~~\n| P1：x | — |\n', False),
            ('mixed-fence-chars-do-not-close',
             table + conclusion + '```\n| P1：x | — |\n~~~\n| more |\n', False),
            # fix-r4: prose or quote lines that carry a pipe but no level are
            # not review rows; they must not fail a clean record.
            ('quote-line-after-table',
             table + conclusion + '> 备注：讨论见 | 评审记录 B。\n', True),
            ('quote-line-with-level-after-table',
             table + conclusion + '> 引用：| P1：x | — |\n', False),
            ('prose-pipe-line',
             table + conclusion + '备注：见附录 | 附录 B。\n', True),
            ('fullwidth-pipe-prose-line',
             table + conclusion + '备注：见附录 ｜ 附录 B。\n', True),
            # fix-r5: a backtick fence with backticks in its info string is
            # prose, not a fence (CommonMark) — rows after it stay visible.
            # fix-r6: closing fence added so the case discriminates — the
            # malformed opening is prose, the poison row after it stays
            # visible (r5 review: without the closer both sides read False).
            ('malformed-fence-info-string', table + conclusion
             + ' ```inline``` 说明\n| P1：x | — |\n```\n', False),
            # fix-r5: a 处置 header column reads like 处理 (CLAUDE.md wording).
            ('chuzhi-header-clean',
             '# 记录\n\n| 轮 | 审查方 | 结论 | 处置 |\n|---|---|---|---|\n'
             '| 1 | DS | 无 P0/P1/P2 | 已修（c1） |\n' + conclusion, True),
            ('chuzhi-header-undisposed',
             '# 记录\n\n| 轮 | 审查方 | 结论 | 处置 |\n|---|---|---|---|\n'
             '| 1 | DS | P1：foo | — |\n' + conclusion, False),
        ]
        for name, body, expected in record_cases:
            with self.subTest(name=name):
                record.write_text(body, encoding='utf-8')
                with patch.object(G, 'WORKTREE', self.root):
                    self.assertIs(check_ruler.independent_review(), expected,
                                  '#2245-r7 record:' + name)
        record.write_bytes(b'# \xff\xfe not utf-8 \n')
        with patch.object(G, 'WORKTREE', self.root):
            self.assertIs(check_ruler.independent_review(), False)
        # The real in-repo record must stay clean under the tightened rule:
        # assert on a byte snapshot, not the live path (r2 concurrency review).
        (record_dir / '001_ruler-recheck.md').write_bytes(
            (G.WORKTREE / 'docs/reviews/001_ruler-recheck.md').read_bytes())
        with patch.object(G, 'WORKTREE', self.root):
            self.assertIs(check_ruler.independent_review(), True)

    def test_pending_and_closure_families_stay_paired(self):
        """#2245 r7 (structural round, both families): closure, failing and
        in-progress families are all derived from the module's verb lists,
        and this check walks those real lists — no hand-copied vocabulary
        that can drift one-sidedly (r5's hand-copied Chinese side let
        验收失败 / 评审不通过 / not 修复 read as closed; r6 asked for module
        constants + traversal, r7 delivered them)."""
        # closure really is derived: gate verbs + English verbs + the wontfix
        # phrases, and nothing else (r6 MM sibling#2). r8 (DS sibling#2 /
        # boundary#5): the old set-equality asserted a `_CLOSED_HANDLING`
        # constant that production code no longer reads, so a one-sided drift
        # stayed green. The vocabulary is now traversed through the real
        # matcher `_handling_closed` — behaviour, not a re-derived definition
        # — and observe-only 观察 never closes.
        for word in (set(check_ruler._CJK_GATE_VERBS)
                     | set(check_ruler._ENGLISH_VERBS)
                     | {'无需', 'wontfix', "won't fix"}):
            self.assertTrue(check_ruler._handling_closed(word),
                            '#2245-r8 closure-word:' + word)
        self.assertFalse(check_ruler._handling_closed('观察'))
        # the failing / in-progress families really are <two-char verb>失败 /
        # <two-char verb>中 over the module's own verb list, derived once.
        self.assertEqual(set(check_ruler._CJK_LONG_VERBS),
                         {v for v in check_ruler._CJK_VERBS if len(v) >= 2})
        # every gate verb keeps every negated/failed/in-progress/intended
        # form on the pending side, in both languages and across them.
        for word in check_ruler._CJK_GATE_VERBS:
            for form in (f'未{word}', f'没{word}', f'无法{word}', f'不能{word}',
                         f'推迟{word}', f'延后{word}', f'待{word}', f'not {word}',
                         f'never {word}'):
                self.assertTrue(check_ruler._handling_pending(form),
                                '#2245-r7 pending-family:' + form)
            if len(word) >= 2:
                self.assertTrue(check_ruler._handling_pending(word + '失败'),
                                '#2245-r7 failing-family:' + word + '失败')
                self.assertTrue(check_ruler._handling_pending(word + '中'),
                                '#2245-r7 in-progress-family:' + word + '中')
                # r7 right edge (DS boundary#3): <verb>中 inside a longer
                # noun is not in-progress — the noun must not reopen a row.
                self.assertFalse(check_ruler._handling_pending(word + '中间层'),
                                '#2245-r7 in-progress-right-edge:' + word + '中间层')
                # r8 (DS boundary#1): the edge blocks only attested noun
                # compounds — an object after 的 keeps the row open.
                self.assertTrue(check_ruler._handling_pending(word + '中的问题'),
                                '#2245-r8 in-progress-object:' + word + '中的问题')
                # r9 (DS boundary#4): the noun compounds after 中 are closed
                # for every preceding verb — one mechanism, one verdict.
                self.assertFalse(check_ruler._handling_pending(word + '中断分析'),
                                '#2245-r9 in-progress-noun:' + word + '中断分析')
                self.assertFalse(check_ruler._handling_pending(word + '中心'),
                                '#2245-r9 in-progress-noun:' + word + '中心')
        for word in check_ruler._ENGLISH_VERBS:
            for form in (f'not {word}', f'never {word}', f'un{word}',
                         f'un-{word}', f'un {word}', f'un_{word}',
                         f"hasn't been {word}", f'未 {word}', f'待 {word}'):
                self.assertTrue(check_ruler._handling_pending(form),
                                '#2245-r7 english-pending:' + form)
        # r8 (DS sibling#3): every negation marker in the module's own list
        # keeps a gate verb pending — the old check only covered 未/没, so a
        # newly added marker without the grammar stayed green.
        for marker in check_ruler._NEGATION_WORDS:
            for word in ('修复', '合并'):
                self.assertTrue(check_ruler._handling_pending(marker + word),
                                '#2245-r8 negation-family:' + marker + word)
        # r8 (DS boundary#2): partial-completion residuals are pending — a
        # cell that names a partial disposal is still open. r9 (DS sibling#2)
        # walks the module's own partial-word list plus the English family.
        for word in check_ruler._PARTIAL_WORDS:
            self.assertTrue(check_ruler._handling_pending('已修复，' + word + '修复'),
                            '#2245-r9 partial-pending:' + word)
        for form in ('修复了一半', '未完全修复', 'partially fixed', 'almost completed',
                     'migrated partially', 'half done', 'nearly finished'):
            self.assertTrue(check_ruler._handling_pending(form),
                            '#2245-r9 en-partial-pending:' + form)
        # r9 (anthropic boundary#9): the 部分 guard blocks only the attested
        # compound — 全部分支 is a closed branch word, 部分修复 still partial.
        # r11 (DS sibling#4): guards split by side — 部分 keeps its left
        # guards (全/影响), 局部 gains the 地区 right guard.
        self.assertIn('部分', check_ruler._PARTIAL_LEFT_GUARDS,
                      '#2245-r9 partial-guard-registered:部分')
        self.assertFalse(check_ruler._handling_pending('已合并全部分支'),
                         '#2245-r9 partial-guard:已合并全部分支')
        self.assertTrue(check_ruler._handling_pending('部分修复'),
                        '#2245-r9 partial-bare:部分修复')
        self.assertFalse(check_ruler._handling_pending('已修复，影响部分用户'),
                         '#2245-r11 partial-guard:影响部分用户')
        self.assertFalse(check_ruler._handling_pending('已修复，局部地区'),
                         '#2245-r11 partial-guard:局部地区')
        self.assertTrue(check_ruler._handling_pending('已修复，局部修复'),
                        '#2245-r11 partial-bare:局部修复')
        # r9 (anthropic boundary#6): every apostrophe-free contraction stem
        # negates a following verb of the module's own English source.
        for stem in check_ruler._EN_CONTRACTIONS:
            self.assertTrue(check_ruler._handling_pending(stem + ' merged'),
                            '#2245-r9 en-contraction:' + stem)
        # the whitelist stays closed: a real word ending in nt is not a
        # negation (want/count/plant must not read pending).
        for word in ('want', 'count', 'plant'):
            self.assertFalse(check_ruler._EN_NEGATION.search(word),
                              '#2245-r9 en-contraction-whitelist:' + word)
        for form in ('verified 100%', 'fixed'):
            self.assertFalse(check_ruler._handling_pending(form),
                             '#2245-r9 full-percentage:' + form)
        for form in ('completed 50%', 'fixed 70%', 'fixed 99%'):
            self.assertTrue(check_ruler._handling_pending(form),
                            '#2245-r9 percentage-pending:' + form)
        # r8 (DS boundary#1 / sibling#3): the explicit-pending right edge
        # blocks only the attested compound — 进行中断分析 is analysis prose,
        # while an object after 的 keeps the row open.
        self.assertFalse(check_ruler._handling_pending('进行中断分析'),
                        '#2245-r8 explicit-pending-edge:进行中断分析')
        for form in ('进行中的评审', '修复中的问题已定位'):
            self.assertTrue(check_ruler._handling_pending(form),
                            '#2245-r8 explicit-pending-object:' + form)
        # r8 (DS sibling#3): the closure guards bind directly — 拒绝/保留意见
        # inside a handling cell must not read as the bare closure verb.
        for form in ('已拒绝', '保留意见'):
            self.assertFalse(check_ruler._handling_closed(form),
                             '#2245-r8 closure-guard:' + form)
        # r9 (DS boundary#5 / boundary#1): the guarded markers bind directly —
        # the module's own guard dictionaries are walked word by word, so a
        # guard dropped from production code fails here, not in a review.
        for word, guard_form, bare_form in (
                ('不通过', '已修复不通过的用例', '评审不通过'),
                ('待办', '已修复待办事项', '待办'),
                ('保留意见', '已处理保留意见变更', '保留意见')):
            self.assertIn(word, check_ruler._PENDING_GUARDS,
                          '#2245-r9 guard-registered:' + word)
            self.assertFalse(check_ruler._handling_pending(guard_form),
                            '#2245-r9 pending-guard:' + guard_form)
            self.assertTrue(check_ruler._handling_pending(bare_form),
                            '#2245-r9 pending-bare:' + bare_form)
        for word, guard_form, bare_form in (
                ('passed', 'passed to next round', 'passed'),
                ('accepted', 'accepted risk', 'accepted')):
            self.assertIn(word, check_ruler._EN_CLOSURE_GUARDS,
                          '#2245-r9 guard-registered:' + word)
            self.assertFalse(check_ruler._handling_closed(guard_form),
                            '#2245-r9 en-closure-guard:' + guard_form)
            self.assertTrue(check_ruler._handling_closed(bare_form),
                            '#2245-r9 en-closure-bare:' + bare_form)
        # DS sibling#2: intent markers are module constants — walk every one
        # of them paired with a gate verb, so adding a marker without the
        # grammar cannot pass silently.
        for marker in check_ruler._INTENT_WORDS:
            for word in ('修复', '验证'):
                self.assertTrue(check_ruler._handling_pending(marker + word),
                                '#2245-r7 intent-marker:' + marker + word)
        # r7 invariant 7: the English pending words are a word-list residual,
        # so each one gets its own traversal assertion (the paired-verb forms
        # above cannot cover them — no verb is involved).
        for form in ('in progress', 'in-progress', 'pending', 'todo', 'tbd',
                     'wip'):
            self.assertTrue(check_ruler._handling_pending(form),
                            '#2245-r7 en-pending:' + form)
        # the guarded markers fire on a bare 需/会 but not inside their
        # compounds (r7 MM boundary#3): 需求方/需求已关闭/会议已通过/有机会.
        for form in ('需修复', '会修复'):
            self.assertTrue(check_ruler._handling_pending(form),
                            '#2245-r7 intent-marker:' + form)
        for guard in ('需求方确认', '需求已关闭', '会议已通过', '有机会再验证'):
            self.assertFalse(check_ruler._handling_pending(guard),
                            '#2245-r7 intent-guard:' + guard)
        # intention markers may look forward across clauses; negation stays
        # in its clause; observe-only verbs never close a row.
        for form in ('计划 12 月 31 日，修复', '暂缓，修复方案见 #100',
                     '已修，待人工验证', '需要修复', '下周修复', '将修复',
                     'will be fixed', 'to be fixed',
                     'not planned for this release, will be fixed later'):
            self.assertTrue(check_ruler._handling_pending(form),
                            '#2245-r7 forward-intent:' + form)
        for closed in ('已修（c1）', '已验收', '已复核', '已合并 to main',
                      '已通过', '已迁移至 #456', "won't fix", '不会修复',
                      '无需处理', '已修，未引入回归', '没发现问题，已修复',
                      '已修复，不需要进一步处理', '已修复，后续观察',
                      '已升级中间件到 2.1', '已关闭中间层缓存', '已保留中间态',
                      '已修改', '已维修', '已检修'):
            self.assertFalse(check_ruler._handling_pending(closed),
                             '#2245-r7 closed-family:' + closed)
        # r14 (DS r13 boundary#4 P0): the AWAIT construction — 待 glued to the
        # word behind it pends on its own, no gate verb needed. The X待 nouns
        # get NO left guard (期待人工验收 is the same outstanding action as
        # 待人工验收); the attested closed compounds (待办, via the r9 pin)
        # ride the right guard.
        for form in ('等待回归', '待回归', '已修复，等待复测',
                     '已修复，期待人工验收'):
            self.assertTrue(check_ruler._handling_pending(form),
                            '#2245-r14 await-construction:' + form)
        self.assertFalse(check_ruler._handling_pending('已修复待办事项'),
                         '#2245-r14 await-guard:已修复待办事项')
        # observe-only verbs and explicit pending words never close a row
        # (r7 MM boundary#8: 待观察 joins its 保留观察/继续观察 family).
        for form in ('观察', '待观察', '继续观察'):
            self.assertFalse(check_ruler._handling_closed(form),
                             '#2245-r7 observe-only:' + form)
        for form in ('待观察', '保留观察', '继续观察', '待定', '待办', '进行中',
                     '拒绝', '拒绝修复', '已拒绝合入', '保留意见'):
            self.assertTrue(check_ruler._handling_pending(form),
                            '#2245-r7 explicit-pending:' + form)
        # r9 (DS boundary#3 / sibling#8): every attested apostrophe code point
        # folds on the judgement copy — the fold runs before NFKC (U+00B4),
        # so a code point dropped from _APOSTROPHES fails this traversal.
        for curly in check_ruler._APOSTROPHES:
            rows = check_ruler._open_review_rows(
                '| 结论 | 处理 |\n|---|---|\n| P1: foo | hasn' + curly + 't been fixed |')
            self.assertTrue(rows is not None and len(rows) == 1,
                            '#2245-r9 apostrophe-fold:' + hex(ord(curly)))
        # r9 (DS sibling#7): the weak negation gap walks its own innocuous
        # set — every member folds 未<c> P1, and a verb-initial or unknown
        # character (定位 / 核对 / 引入) keeps the level a real finding.
        # r9 (DS boundary#2 / sibling#3): the weak negation tier folds only
        # across the attested modifier characters — every member folds a bare
        # level, and an unattested character (定位/核对/引入) keeps the level
        # a real finding (fail-closed). The set is pinned by equality: it is
        # a curated allowlist, not a derivation, so any edit must re-attest
        # here (the per-character loop below reads the module's own set and
        # cannot see a shrink).
        # r10 (MM boundary#4): 有 joins the modifier set — 未有任何 P1 folds
        # like 无任何 P1, and the frozen literal below re-attests the whole
        # set (a live per-character loop cannot see a deletion).
        self.assertEqual(check_ruler._NEG_GAP_INNOCUOUS,
                         '新的增严重级别其他它任何有',
                         '#2245-r10 innocuous-set-frozen')
        for char in check_ruler._NEG_GAP_INNOCUOUS:
            self.assertIsNotNone(check_ruler._NEGATED_LEVELS.search('未' + char + ' P1'),
                                 '#2245-r9 innocuous-gap-fold:' + char)
        for prose in ('尚未定位 P1', '尚未核对 P1', '未引入 P1'):
            self.assertIsNone(check_ruler._NEGATED_LEVELS.search(prose),
                             '#2245-r9 innocuous-gap-survive:' + prose)
        # r9 (DS sibling#7): every negation marker keeps a gate verb pending
        # through the full pipeline. The live-list traversal above cannot see
        # a deletion (the shrunk list simply stops iterating), so the
        # attested marker set is also walked from this fixed tuple — deleting
        # a marker (正在) fails the frozen walk, not the live one.
        for marker in check_ruler._NEGATION_WORDS:
            self.assertTrue(check_ruler._handling_pending(marker + '修复'),
                            '#2245-r9 negation-marker-pipeline:' + marker)
        for marker in ('未', '没', '尚未', '暂不', '正在'):
            self.assertIn(marker, check_ruler._NEGATION_WORDS,
                          '#2245-r9 negation-marker-frozen:' + marker)
            self.assertTrue(check_ruler._handling_pending(marker + '修复'),
                            '#2245-r9 negation-marker-frozen:' + marker)
        # r10 (polarity round): every r10 vocabulary element is pinned by a
        # FIXED literal — a shrink of the module's own constant is invisible
        # to a live traversal (the self-source blindness r9 hit on
        # _NEGATION_WORDS, MM sibling#3 on the English partials, MM
        # sibling#6 on the guard dictionaries).
        self.assertEqual(check_ruler._EN_CONTRACTIONS,
                         ('hasnt', 'isnt', 'wasnt', 'doesnt', 'didnt', 'arent',
                          'werent', 'couldnt', 'wouldnt', 'shouldnt',
                          'havent', 'dont', 'cant', 'mustnt', 'neednt',
                          'shant', 'aint', 'wont'),
                         '#2245-r10 contractions-frozen')
        self.assertEqual(check_ruler._EN_PARTIAL_WORDS,
                         ('partially', 'partial', 'partly', 'almost', 'nearly',
                          'halfway', 'half', 'incomplete', 'unfinished'),
                         '#2245-r11 en-partial-words-frozen')
        self.assertEqual(check_ruler._EN_PARTIAL_GUARDS,
                         {'almost': r'(?!\s*no\b)', 'nearly': r'(?!\s*no\b)',
                          'half': r'(?!-open\b)'},
                         '#2245-r12 en-partial-guard-keys-frozen')
        # r14 (DS r13 sibling#4②): the English partial LEFT guards are their
        # own dict — the right-guard dict is appended after the word, where a
        # lookbehind can never see past the word it follows (the first r14
        # attempt placed them there and the guard silently matched nothing).
        # r15 (DS r14 sibling#3 P1 / boundary#4 P2): the guard covers every
        # non-exclusion member of the family with the full inflected
        # impact/affect set (impacted / affected / impacts on join).
        self.assertEqual(check_ruler._EN_PARTIAL_LEFT_GUARDS,
                         {word: (r'(?<!impact\son\s)(?<!impacts\son\s)'
                                 r'(?<!impact\s)(?<!impacts\s)'
                                 r'(?<!impacting\s)(?<!impacted\s)'
                                 r'(?<!affect\s)(?<!affects\s)'
                                 r'(?<!affecting\s)(?<!affected\s)')
                          for word in ('partially', 'partial', 'partly',
                                       'halfway', 'half', 'incomplete',
                                       'unfinished')},
                         '#2245-r15 en-partial-left-guards-frozen')
        self.assertEqual(check_ruler._CJK_COMPOUND_RULES,
                         ('断(?=分析|场景|的|$)', '心(?=化|$)', '文(?=档)',
                          '层(?=的|$)', '期(?=指标)', '端(?=节)', '部(?=审计)',
                          '档(?=配置)', '国(?=区)', '华(?=分支)', '台(?=服务)',
                          '控(?=服务)', '南(?=区)', '西(?=部)',
                          '间(?=件|层|态)', '等(?=[级严优])'),
                         '#2245-r14 compound-rules-frozen')
        # r12: the residual count-word class is a fixed literal too — a
        # dropped numeral (两) is invisible to a live char-class walk.
        # r13 (DS b#3 P0): the quantifier tail 若干 joins the frozen class.
        self.assertEqual(check_ruler._CJK_COUNT_WORDS,
                         '1-9一二三四五六七八九十百千万两几多数待若干',
                         '#2245-r13 residual-count-words-frozen')
        # r13 (DS s#5/b#5 P2): the residual noun set and the pct extra nouns
        # are the same self-source blindness — a dropped noun (缺陷) or a
        # dropped progress noun (完成度) never shows in a live walk.
        self.assertEqual(check_ruler._CJK_RESIDUAL_NOUNS,
                         'P[012]|问题|风险|隐患|缺陷|事项|用例|场景|脚本',
                         '#2245-r14 residual-nouns-frozen')
        self.assertEqual(check_ruler._CJK_PCT_EXTRA,
                         ('进度', '完成度'),
                         '#2245-r13 pct-extra-frozen')
        # r12 (DS sibling#10 P2): the intent markers and their guard dict are
        # the same self-source blindness — a dropped marker (将) simply stops
        # the live iteration without failing anything, so the attested set
        # is pinned as a fixed literal, negation-marker-frozen style.
        self.assertEqual(check_ruler._INTENT_WORDS,
                         ('待', '计划', '拟', '暂缓', '暂时', '暂且', '无法',
                          '不能', '推迟', '延后', '后续', '稍后', '下周', '本周',
                          '需', '会', '将'),
                         '#2245-r12 intent-words-frozen')
        self.assertEqual(check_ruler._MARKER_GUARDS,
                         {'需': r'(?<![无不])需(?!求)',
                          '会': r'(?<!不)(?<![机社体])会(?!议|话)',
                          '待': r'待(?!办|遇)'},
                         '#2245-r14 marker-guards-frozen')
        self.assertEqual(set(check_ruler._PENDING_GUARDS),
                         {'不通过', '待办', '保留意见'},
                         '#2245-r10 pending-guard-keys-frozen')
        self.assertEqual(set(check_ruler._EN_CLOSURE_GUARDS),
                         {'passed', 'accepted'},
                         '#2245-r10 en-closure-guard-keys-frozen')
        # r13 (DS b#1 P0): the accepted guard's bounded token window was a
        # fixed literal. r14 (DS b#2 / MM b#1 P0, MM s#2 / DS s#2 P1): the
        # window became CLAUSE-SCOPE over the shared separator family — the
        # literal re-attests the whole shape (a shrunk window or a lost
        # separator member resurfaces here, not in a future review).
        # r15 (DS r14 boundary#2 P1 / concurrency#3 P2): the family takes
        # the ASCII sentence punctuation and the hyphen, and the separator
        # runs are POSSESSIVE (the two adjacent separator consumers were a
        # quadratic split tree — measured 9.1 s at 20000 commas).
        self.assertEqual(check_ruler._EN_CLOSURE_GUARDS['accepted'],
                         r'(?![\s,;:，。、；：！？.!?/()\[\]-]*+(?:'
                         r'(?:[^\s,;:，。、；：！？.!?/()\[\]-]+'
                         r'[\s,;:，。、；：！？.!?/()\[\]-]++)*'
                         r'[\s,;:，。、；：！？.!?/()\[\]-]*+risks?\b'
                         r'|for\s+now\b))',
                         '#2245-r15 accepted-guard-window-frozen')
        # r15 (DS r14 sibling#1 P1, MM r13 boundary#6 P2): 'passed' gains the
        # for-now deferral branch on the same possessive separator family.
        self.assertEqual(check_ruler._EN_CLOSURE_GUARDS['passed'],
                         r'(?![\s,;:，。、；：！？.!?/()\[\]-]*+'
                         r'(?:(?:on(?:\s+to)?|onto|to)\b|for\s+now\b))',
                         '#2245-r15 passed-guard-deferral-frozen')
        # r13: the disposal filter's clause-break family is a fixed literal
        # too — a widened set (a letter reaching it) would re-close the r11
        # pin 已修复中等待回归, and a dropped separator would re-pend the
        # b#4 disposal shapes. r14 (DS b#5 / MM b#3 P1): the family takes
        # brackets, ASCII period and slash. r15 (DS r14 boundary#2 P1): the
        # square-bracket family joins (已修复中断[回归通过] was a false FAIL).
        self.assertEqual(check_ruler._CJK_CLAUSE_BREAKS,
                         '，。、；：！？,;:!? \t.()（）/[]【】「」',
                         '#2245-r15 clause-breaks-frozen')
        self.assertEqual(set(check_ruler._PARTIAL_LEFT_GUARDS),
                         {'部分', '局部', '过半', '一半', '阶段性', '未完全'},
                         '#2245-r14 partial-left-guard-keys-frozen')
        self.assertEqual(set(check_ruler._PARTIAL_RIGHT_GUARDS), {'局部'},
                         '#2245-r11 partial-right-guard-keys-frozen')
        # r11 (DS boundary#1 + sibling#1 / MM boundary#1 + sibling#4+#5, both
        # lineages, P0): the percentage verbs are DERIVED from the closure
        # word source — the r10 table was a third hand copy whose Chinese
        # side carried 8 of 27 closure verbs. The derivation is pinned from
        # BOTH ends: the word sources are frozen as literals (an edit to
        # _ENGLISH_VERBS / _CJK_VERBS must re-attest here), and the derived
        # set must equal gate verbs + the two progress nouns + the English
        # table, so reintroducing a hand copy fails this cross-walk.
        self.assertEqual(check_ruler._ENGLISH_VERBS,
                         ('done', 'fixed', 'resolved', 'closed', 'merged',
                          'verified', 'confirmed', 'reviewed', 'tested',
                          'checked', 'accepted', 'migrated', 'passed',
                          'completed', 'modified', 'upgraded', 'retested',
                          'rechecked', 'downgraded', 'handled'),
                         '#2245-r11 english-verbs-frozen')
        self.assertEqual(check_ruler._CJK_VERBS,
                         ('修', '修复', '解决', '关闭', '拒', '保留', '完成',
                          '合入', '并入', '合并', '降级', '升级', '处置', '处理',
                          '评审', '复核', '验收', '通过', '验证', '确认', '检查',
                          '测试', '复检', '迁移', '观察', '修改', '维修', '检修'),
                         '#2245-r11 cjk-verbs-frozen')
        self.assertEqual(check_ruler._PCT_COMPLETION_VERBS,
                         check_ruler._CJK_GATE_VERBS + check_ruler._CJK_PCT_EXTRA
                         + check_ruler._ENGLISH_VERBS,
                         '#2245-r11 pct-verbs-derived')
        # the wont distinction keeps its closure spellings and only those:
        # the apostrophe phrase and the one-word wontfix close, "won't be
        # fixed" / "wont be fixed" stay pending, bare "wont fix" is the
        # fail-closed known boundary (reads open).
        self.assertTrue(check_ruler._handling_closed("won't fix"),
                        '#2245-r10 wont-closure-phrase')
        self.assertTrue(check_ruler._handling_closed('wontfix'),
                        '#2245-r10 wont-closure-word')
        self.assertTrue(check_ruler._handling_pending("won't be fixed"),
                        '#2245-r10 wont-be-fixed-open')
        self.assertTrue(check_ruler._handling_pending('wont be fixed'),
                        '#2245-r10 wont-be-fixed-open')
        self.assertFalse(check_ruler._handling_pending('wont fix'),
                         '#2245-r10 wont-fix-known-boundary')
        self.assertFalse(check_ruler._handling_closed('wont fix'),
                         '#2245-r10 wont-fix-known-boundary')
        # the right-extension guard on \w+n't is load-bearing: "won't fix"
        # must not pend through a later verb of the same clause (brackets do
        # not split clauses), while "won't be fixed" still pends.
        self.assertFalse(
            check_ruler._handling_pending("won't fix (verified elsewhere)"),
            '#2245-r10 wontfix-guard')
        # every compound rule keeps its ATTESTED shape closed — one real word
        # per rule. The r10 loop walked '已修复中' + character, which pinned
        # unattested spellings (已修复中间 / 已修复中等 are not words) and let
        # the guarded characters' bug shapes pass (DS boundary#1: 已修复中等
        # itself was the false PASS). r11 walks the guarded characters'
        # failure shapes too: a guard only closes in its attested shape.
        for form in ('已修复中断场景', '已完成中心化改造', '已修复中文档',
                     '已升级中间层', '已保留中间态', '评审中期指标',
                     '已升级中端节点', '已完成中部审计', '已修复中档配置',
                     '已修复中等严重', '已升级中国区网关', '已合并中华分支',
                     '已修复中台服务', '已修复中控服务', '已修复中南区网关',
                     '已修复中西部节点', '已升级中间件', '已关闭中断',
                     '已修复中断的会话', '已修，进行中断分析',
                     '已升级中层的节点'):
            self.assertFalse(check_ruler._handling_pending(form),
                            '#2245-r11 compound-attested:' + form)
        for form in ('修复中等待回归', '修复中等待 CI', '修复中期评估',
                     '已修复，修复中间发现新问题', '已修复中毒', '修复中（',
                     '修复中/', '修复中发现新问题', '修复  中', '修复   中',
                     '修复中断，等待回归', '修复中部署新版本', '修复中控制变量',
                     '修复中端详日志', '修复中端掉旧服务', '修复中层叠缓存',
                     '已修复中断回归通过',
                     # r14: the eight previously bare single characters close
                     # only on their attested continuations — anything else
                     # after 中 reopens the row.
                     '修复中心跳异常', '修复中台账异常', '修复中文化说明',
                     '修复中档位排序', '修复中国产化脚本', '修复中华文化样式',
                     '修复中南方向问题', '修复中西侧节点'):
            self.assertTrue(check_ruler._handling_pending(form),
                            '#2245-r11 compound-guarded:' + form)
        # every attested English partial word keeps the row open on its own.
        for word in check_ruler._EN_PARTIAL_WORDS:
            self.assertTrue(check_ruler._handling_pending(word),
                            '#2245-r10 en-partial-bare:' + word)
        # every completion verb anchors a sub-100 percentage and never
        # anchors a full one, whatever its spelling.
        for verb in check_ruler._PCT_COMPLETION_VERBS:
            self.assertTrue(check_ruler._handling_pending(verb + ' 50%'),
                            '#2245-r10 pct-pending:' + verb)
            self.assertFalse(check_ruler._handling_pending(verb + ' 100%'),
                            '#2245-r10 pct-full:' + verb)
        # r11 (DS boundary#1 / MM boundary#1, P0): the frozen side of the
        # same walk — the live loop above reads the module's own derived set
        # and cannot see a verb dropped from the word source. Every gate
        # verb must anchor verb 50% (the r10 hand copy missed 19 of them).
        for verb in ('修', '修复', '解决', '关闭', '拒', '保留', '完成', '合入',
                     '并入', '合并', '降级', '升级', '处置', '处理', '评审',
                     '复核', '验收', '通过', '验证', '确认', '检查', '测试',
                     '复检', '迁移', '修改', '维修', '检修'):
            self.assertTrue(check_ruler._handling_pending(verb + ' 50%'),
                            '#2245-r11 pct-cjk-frozen:' + verb)
        for form in ('completed 50 percent', 'completed 50 %', 'fixed 99.5%',
                     '修复了 70%', '进度 80%', '已完成度 80%',
                     'completed 300 of 500 (60%)', 'fixed after retesting 50%',
                     'completed 3000 of 50000 (60%)'):
            self.assertTrue(check_ruler._handling_pending(form),
                            '#2245-r10 pct-spelling:' + form)
        for form in ('verified 100.0%', 'verified 1000%'):
            self.assertFalse(check_ruler._handling_pending(form),
                             '#2245-r10 pct-full-spelling:' + form)
        # a rate metric, a coverage figure, a leftover count and an impact
        # note never anchor — the r9 false-FAIL family stays fixed. 修复率 /
        # passed rate carry a completion verb directly before the rate word,
        # so only the 率/rate gap exclusion keeps them from anchoring.
        for form in ('已修复，回归通过率 98%', '已修复，覆盖率 95%',
                     '已修复，pass rate 98%', '已修复，遗留 0%',
                     '已修复（影响 30% 用户）', '已合并，修复率 98%',
                     '已修复，passed rate 98%'):
            self.assertFalse(check_ruler._handling_pending(form),
                            '#2245-r10 pct-rate:' + form)
        # a residual quantity reopens, a negated residual does not.
        for form in ('已修复不通过的用例，还有 3 个', '已合并，剩余 2 项',
                     '已处置，仍有 1 项', '已修，还剩 5 个', '已修，另有 2 项'):
            self.assertTrue(check_ruler._handling_pending(form),
                            '#2245-r10 residual-pending:' + form)
        for form in ('已修，无剩余', '已修，没有剩余'):
            self.assertFalse(check_ruler._handling_pending(form),
                             '#2245-r10 residual-negated:' + form)
        # r11 (MM boundary#5 + sibling#2 / DS sibling#5 + boundary#6, both
        # lineages, P1): the residual family is count-anchored and
        # bilingual — the quantifier, the noun set and the English mirror
        # are walked from fixed literals so a shrink of any of them fails
        # here, not in a review.
        # r12 (DS b#1 + b#2 P0 / s#2 + s#5 + s#6 P1 / b#5 + b#6): the noun
        # set rides the whole quantifier family — 另有风险 flips to pending
        # (the r11 walk below pinned the closed side of the regression
        # itself), the numeral class covers 两/多/数, the English side
        # excludes zero, takes a bounded noun gap and the prepend form.
        for form in ('已修复，还有 3 个', '已修复，还剩 5 个', '已修复，仍有 1 项',
                     '已修复，剩余 2 项', '已修复，尚余 2 个', '已修复，余下 3 个',
                     '已修复，另有 2 项', '已修复，遗留 2 项', '已修复，遗留问题',
                     'fixed, 3 remaining', 'fixed, 3 outstanding',
                     'fixed, 3 left', 'still 3 unresolved, fixed',
                     '已修复，还有待回归',
                     '已修复，还有问题', '已修复，仍有风险', '已修复，还有隐患',
                     '已修复，另有风险', '已修复，还有两处未覆盖',
                     '已修复，还有两个问题', '已修复，还有多个用例',
                     '已修复，还有数处', 'fixed, 3 issues remaining',
                     'fixed, 3 findings left', 'fixed, remaining 3',
                     '已修复，还有若干处未覆盖', '已修复，还有少量问题',
                     '已修复，还有较多未覆盖用例', '已修复，还有不少问题',
                     '已修复，还有：3 个', '已修复，还有缺陷',
                     '已修复，遗留隐患', 'fixed, 3 issues remain',
                     'fixed, still issues remain', 'still 3 issues remain'):
            self.assertTrue(check_ruler._handling_pending(form),
                            '#2245-r11 residual-family:' + form)
        for form in ('已修，无剩余', '已修，没有剩余', '已修，还有 0 个',
                     '已合并，还剩 0 项', '已修复，另有说明',
                     '已修复，遗留 0%', '已修，无遗留', '已修，无任何剩余',
                     '已修复，剩余风险已接受', '已修复，没有遗留问题',
                     '已修，fixed, 0 remaining', '已修，still 0 open',
                     '已修，still 0 issues remain', 'fixed, 0 issues remain',
                     '已修复，还有别的说明', 'fixed, issues remain',
                     '已修复中断，回归通过', '已关闭中断，后续观察',
                     '已修复，影响局部用户', '已修复，影响过半用户',
                     '已修复，影响一半用户'):
            self.assertFalse(check_ruler._handling_pending(form),
                            '#2245-r11 residual-anchor-guards:' + form)
        # the closure guards block their whole attested extension family
        # while the bare verbs still close.
        for form in ('accepted the risk', 'accepted as a risk',
                     'accepted as risk', 'accepted with risk',
                     'passed on to next round', 'passed onto next round',
                     'accepted, risk noted', 'accepted for now',
                     'passed, to next round',
                     'accepted the remaining risks', 'accepted all of the risks',
                     'accepted three risks', 'accepted most risks',
                     'accepted some risks', 'accepted the two risks',
                     'accepted as a residual risk', 'accepted; risk noted'):
            self.assertFalse(check_ruler._handling_closed(form),
                            '#2245-r10 en-closure-guard-family:' + form)
        for form in ('accepted', 'accepted the change', 'passed the review'):
            self.assertTrue(check_ruler._handling_closed(form),
                            '#2245-r10 en-closure-bare-family:' + form)
        # r14 (DS r13 concurrency#1 + MM r13 concurrency#1, both lineages
        # independently, P0 hang): the closure-guard family — the shared
        # separator run reaches a risk noun across the FULL clause (the
        # attested-escape shapes r13 missed: past the 4th word unit, glued
        # to punctuation, behind a full-width separator) and 'passed' rides
        # the same family (MM sibling#1: passed; to next round read CLOSED
        # while the accepted mirror did not).
        for form in ('accepted all of the remaining open risks',
                     'accepted with the caveat that the risk remains',
                     'accepted in spite of the remaining risks',
                     'accepted (risk noted)', 'accepted/risk noted',
                     'accepted，risk noted', 'passed; to next round'):
            self.assertFalse(check_ruler._handling_closed(form),
                            '#2245-r14 en-closure-guard-family:' + form)

    def test_pct_gap_backtracks_linearly(self):
        """#2245 r14 (DS r13 concurrency#1 + MM r13 concurrency#1, both
        lineages, the SAME finding, measured live): the r13 unit
        alternation OVERLAPPED — the fallback branch also matched digits and
        letters and a non-possessive \\d+ / [A-Za-z]+ can split one run into
        several units, so a long alnum token with no % forced the engine
        through every split tree (measured pre-r14: 0.07 s at L=16, 1.25 s
        at L=24, 4.89 s at L=32, exponential — a pasted sha256 in a
        handling cell hangs the ruler = P0). The units are now possessive
        and mutually exclusive, so each position has exactly one parse.
        This control pins the LINEAR behavior with a hard wall-clock bound —
        a reintroduced overlap (a lost possessive or a fallback class that
        leaks digits) does not produce a wrong verdict, it hangs, so only a
        timer catches it.
        r15 (DS r14 boundary#3 P2 / boundary#5 P2 / concurrency#1+#2 P2):
        the timer itself is load-robust — the SIGALRM handler is SAVED and
        restored via addCleanup (a leaked handler poisons later tests), the
        inputs are sized so the measured span sits at ~0.3-1.5 ms (the r14
        0.1 ms spans rode the timing noise floor and the scaling ratio
        could flip on scheduler jitter alone), each measurement is a MIN
        of three runs, and the ratio denominator has a 0.2 ms floor. The
        SIGALRM timer does interrupt the C-level backtracking loop (r15
        probe, CPython 3.14.3: TimeoutError after 0.303 s mid-backtrack,
        raw output in _tmp/20261110-issue-2245-closure/fix-r15/) so the
        wall-clock bound fails the test instead of hanging it. The Unicode
        Nd case pins the r15 single-branch fix (Arabic-Indic digits matched
        BOTH \\d++ and the fallback class pre-r15: 0.004 s at L=28,
        0.018 s at L=32, doubling per 4 chars — a different alphabet
        replayed the r14 exponential)."""
        def _hang(signum, frame):
            raise TimeoutError('pct-gap hang: >5s')
        prev_handler = signal.signal(signal.SIGALRM, _hang)
        self.addCleanup(signal.signal, signal.SIGALRM, prev_handler)

        def _timed(cell):
            signal.setitimer(signal.ITIMER_REAL, 5.0)
            try:
                t0 = time.monotonic()
                check_ruler._handling_pending(cell)
                return time.monotonic() - t0
            finally:
                signal.setitimer(signal.ITIMER_REAL, 0)

        def _timed_min3(cell):
            return min(_timed(cell) for _ in range(3))

        cell = 'fixed, verified ' + '0123456789abcdef' * 32
        try:
            elapsed = _timed_min3(cell)
        except TimeoutError:
            self.fail('#2245-r15 pct-gap-linear (hang: >5s)')
        self.assertLess(elapsed, 1.0, '#2245-r15 pct-gap-linear')
        # the linear claim is length-independent: 4x the token, ~4x the
        # time. A quadratic or exponential regression fails the ratio even
        # when both runs stay under the absolute bound (floor: 0.2 ms, so
        # a sub-noise denominator cannot flip the verdict on jitter).
        cell2 = 'fixed, verified ' + '0123456789abcdef' * 128
        try:
            elapsed2 = _timed_min3(cell2)
        except TimeoutError:
            self.fail('#2245-r15 pct-gap-linear-long (hang: >5s)')
        self.assertLess(elapsed2, 1.0, '#2245-r15 pct-gap-linear-long')
        self.assertLess(elapsed2 / max(elapsed, 2e-4), 8.0,
                        '#2245-r15 pct-gap-scaling')
        # the Unicode Nd token rides the same wall clock — the r15 fix made
        # every Nd position a single \\d++ parse, so the shape that measured
        # exponential pre-r15 must stay under the same absolute bound.
        cell3 = 'fixed, verified ' + '١a٢b٣c٤d' * 16
        try:
            elapsed3 = _timed_min3(cell3)
        except TimeoutError:
            self.fail('#2245-r15 pct-gap-unicode-nd (hang: >5s)')
        self.assertLess(elapsed3, 1.0, '#2245-r15 pct-gap-unicode-nd')

    def test_separator_runs_backtrack_linearly(self):
        """#2245 r15 (DS r14 concurrency#3 P2 + sibling#5 P1 / boundary#2
        P2, all measured live on 87871a970, raw output in
        _tmp/20261110-issue-2245-closure/fix-r15/raw/probe-r15-timing.out):
        three quadratic regressions hid behind long separator runs —
        (1) the closure-guard separator runs were GREEDY, so the outer run
        and the trailing run split one comma run through an empty word
        group (measured: 1.07 s at 20k commas, 4.1 s at 40k, 9.2 s at 80k —
        quadratic; possessive now, one parse); (2) the residual lookahead
        carried TWO space consumers (\\s*[：:]?\\s* before a [^clause]* that
        also matches spaces — measured: 379.6 s at 5k spaces, >25 s at 20k;
        the leading run is possessive and the second \\s* is gone); (3) the
        marker loop's forward scan re-scanned every later clause per clause
        (measured: 7.5 s at 20k commas; one backward suffix pass now). No
        verdict changes anywhere — only wall clock — so, like the pct
        control above, only a timer catches a regression. The SIGALRM
        interrupt of the C-level loop is verified (r15 probe: TimeoutError
        after 0.303 s mid-backtrack)."""
        def _hang(signum, frame):
            raise TimeoutError('separator-run hang: >5s')
        prev_handler = signal.signal(signal.SIGALRM, _hang)
        self.addCleanup(signal.signal, signal.SIGALRM, prev_handler)

        def _timed(fn, cell):
            signal.setitimer(signal.ITIMER_REAL, 5.0)
            try:
                t0 = time.monotonic()
                fn(cell)
                return time.monotonic() - t0
            finally:
                signal.setitimer(signal.ITIMER_REAL, 0)

        # (1) closure-guard separators: _handling_closed on a 120k-comma
        # cell took ~20 s greedy (quadratic extrapolation of the measured
        # curve), 0.5 ms possessive.
        try:
            elapsed = _timed(check_ruler._handling_closed,
                            check_ruler._normalized_handling(
                                'accepted' + ',' * 120000))
        except TimeoutError:
            self.fail('#2245-r15 sep-run-linear-closure (hang: >5s)')
        self.assertLess(elapsed, 1.0, '#2245-r15 sep-run-linear-closure')
        # (2) the residual lookahead's leading space run: the double
        # consumer measured 379.6 s at 5k spaces (a super-quadratic split
        # tree), 15 ms single-parse.
        try:
            elapsed = _timed(check_ruler._handling_pending,
                            check_ruler._normalized_handling(
                                '已修复，还有' + ' ' * 20000 + 'x'))
        except TimeoutError:
            self.fail('#2245-r15 sep-run-linear-residual (hang: >5s)')
        self.assertLess(elapsed, 1.0, '#2245-r15 sep-run-linear-residual')
        # (3) the marker-loop forward scan: 7.5 s at 20k commas pre-fix,
        # 37 ms with the one-pass suffix precompute (30k commas here for
        # margin).
        try:
            elapsed = _timed(check_ruler._handling_pending,
                            check_ruler._normalized_handling(
                                'accepted' + ',' * 30000))
        except TimeoutError:
            self.fail('#2245-r15 sep-run-linear-forward (hang: >5s)')
        self.assertLess(elapsed, 1.0, '#2245-r15 sep-run-linear-forward')


if __name__ == '__main__':
    unittest.main(verbosity=2)
