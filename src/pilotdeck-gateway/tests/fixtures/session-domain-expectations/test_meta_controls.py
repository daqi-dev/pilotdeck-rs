#!/usr/bin/env python3
# anchor-selfcheck: unsupported in-memory unittest coverage; no source replacements
"""#1190 binding, read-only listing and evidence regressions.

The historical filename remains for callers; the document-scanning meta
control runner and its tests have been removed. Function variants below run
only in memory, compile before testing, and bind exact unittest diagnostics.
#1279 adds the standalone ``--check-anchors`` invocation (no ``--list``):
its ``or not args.list`` half-branch is the only thing that returns 0 from
the preflight without ``--output`` / ``--source``.
"""
import contextlib
import copy
import io
import inspect
import json
import os
from pathlib import Path
import sys
import unittest
from unittest.mock import MagicMock, Mock, patch

sys.dont_write_bytecode = True
sys.path.insert(0, str(Path(__file__).resolve().parent))
import negative_controls as gate
import negative_process


class _RecordingTreeLock:
    """#2795 fix-r4（评审建议①）：真 TreeLock 会 os.open(O_RDWR|O_CREAT) 落
    ``.negative-source.lock``——被下面的只读审计钩子判写（#1279 standalone
    红过），且让本套件依赖锁文件空闲。与 test_negative_roster_guards.py 的
    ``_RecordingTreeLock`` 同形：记录锁获取/释放与读的相对顺序，只验「读面在
    锁内」的语义，不落真锁文件。fix-r5 加 ``busy``：``__enter__`` 抛
    TimeoutError，驱动 setUpClass 的锁忙拒跑形态（名册驱动器同形）。"""

    def __init__(self, events, *, busy=False):
        self.events = events
        self.busy = busy
        self.held = False
        self.calls = []

    def __call__(self, *args, **kwargs):
        self.calls.append((args, kwargs))
        return self

    def __enter__(self):
        if self.busy:
            raise TimeoutError('setup-class drive: lock timeout (#2795 fix-r5)')
        self.events.append('lock-enter')
        self.held = True
        return self

    def __exit__(self, *_exc):
        self.held = False
        self.events.append('lock-exit')
        return False


def _pinned_anchor_reads(events, lock):
    """Patch ``Path.read_text`` so every anchor read records lock-held state.

    A read outside the lock window is the race fix-r3 closed (a sibling
    runner's mutant between its write and restore phases), so it lands in
    ``events`` as an ``unlocked-read`` the assertions can reject.
    """
    real_read_text = Path.read_text

    def recording_read_text(self, *args, **kwargs):
        if not lock.held:
            events.append(('unlocked-read', str(self)))
        return real_read_text(self, *args, **kwargs)

    return patch.object(Path, 'read_text', recording_read_text)


class DriftTests(unittest.TestCase):
    def setUp(self):
        # #2795 fix-r4（评审建议①）：main() 的预检（panel_table）与 check_anchors
        # 都取真树锁——真 TreeLock 会 os.open(O_RDWR|O_CREAT) 落锁文件，被
        # test_list_is_read_only / test_standalone_check_anchors 的只读审计钩子
        # 判写，也让整套件依赖 .negative-source.lock 空闲。统一换成记录桩；
        # 「读面在锁内」由 _pinned_anchor_reads 单测钉住。
        # #2795 fix-r6（评审 concurrency#2）：本桩只覆盖实例方法（setUp 之后）；
        # setUpClass 的锁面见 _meta_class_setup（元控入口）与
        # SetupClassReadSurface（真入口机检）。
        self.lock_events = []
        self.lock = _RecordingTreeLock(self.lock_events)
        patcher = patch.object(gate, 'tree_lock', self.lock)
        patcher.start()
        self.addCleanup(patcher.stop)

    @classmethod
    def setUpClass(cls):
        # #2795 fix-r5（评审 concurrency P2#1 升格 + simplify P2#1 结构性收口）：
        # setUpClass 早于 setUp 的记录桩 / 读面补丁，此前两处 panel_table 读在
        # 锁外——gateway-entry-guard 等兄弟变异轮写 controls/session_domain.py 的
        # write 与 restore 之间，无锁 ast.parse 会把变异体当权威表。与 main()
        # 预检同面，改走共享 anchor_read_window 装置（锁内读 + rc 2 拒跑）；
        # 机检见 SetupClassReadSurface。
        # #2795 fix-r6（评审 concurrency#2）：真入口（unittest.main）保持真锁：
        # 锁忙 → 装置 SystemExit(2)，实测（Python 3.12.3）确认 SystemExit 穿过
        # unittest._handleClassSetUp（只捕 Exception）整进程退出 rc 2、无报告，
        # 是既定的 fail-closed 形态；元控 --negative 入口不取真锁，
        # 见 _meta_class_setup。
        with negative_process.anchor_read_window(
                gate.ROOT, 'session-domain-expectations meta-controls'):
            cls.expectations = gate.panel_table('EXPECTATION_CASES')
            cls.calibration = gate.panel_table('CALIBRATION_CASES')

    def test_binding_corruptions_are_rejected(self):
        state = 'drop-registry-state-require'
        row = next(row for row in gate.MUTATIONS if row[0] == state)
        scenarios = (
            ('deleted', f'BOUND names unknown mutation {state}'),
            ('empty', f'BOUND[{state}] is empty'),
            ('malformed', f'{state}: BOUND entry is not a (case, rule) pair'),
            ('non-sequence', f'BOUND[{state}] is not a sequence'),
            ('short-row', 'MUTATIONS row is not a (name, source, old, new) row'),
            ('duplicate', f'MUTATIONS names are not unique: {state}'),
        )
        for name, marker in scenarios:
            with self.subTest(name=name):
                require_rows, bound = copy.deepcopy(gate.MUTATIONS), copy.deepcopy(gate.BOUND)
                self.assertFalse(gate.check_bindings(self.expectations, require_rows, bound, gate.UNAFFECTED),
                                 '#1190 bindings: unchanged tables must pass')
                if name == 'deleted':
                    require_rows.remove(row)
                elif name == 'empty':
                    bound[state] = ()
                elif name == 'malformed':
                    bound[state] = (('expectations-rust-registry-state',),)
                elif name == 'non-sequence':
                    bound[state] = 42
                elif name == 'short-row':
                    require_rows[require_rows.index(row)] = row[:-1]
                else:
                    require_rows.append(row)
                errors = gate.check_bindings(self.expectations, require_rows, bound, gate.UNAFFECTED)
                self.assertTrue(any(error.startswith(marker) for error in errors),
                                '#1190 bindings: corruption must have its bound diagnostic')

    def test_wrong_rule_binding_is_rejected(self):
        state = 'drop-registry-state-require'
        for table in ('BOUND', 'UNAFFECTED'):
            with self.subTest(table=table):
                bound, unaffected = copy.deepcopy(gate.BOUND), copy.deepcopy(gate.UNAFFECTED)
                self.assertFalse(gate.check_bindings(self.expectations, gate.MUTATIONS, bound, unaffected),
                                 '#1190 wrong-rule: unchanged bindings must pass')
                case, rule = bound[state][0] if table == 'BOUND' else unaffected[state]
                wrong = rule + '_WRONG'
                if table == 'BOUND':
                    bound[state] = ((case, wrong),)
                else:
                    unaffected[state] = (case, wrong)
                errors = gate.check_bindings(self.expectations, gate.MUTATIONS, bound, unaffected)
                marker = f'{state}: {case} is claimed as {wrong} but registered as {rule}'
                self.assertTrue(marker in errors,
                                '#1190 wrong-rule: registered rule mismatch must be rejected')

    def test_list_reports_live_tables_and_counts(self):
        for extended in (False, True):
            with self.subTest(extended=extended):
                expectations, calibration = dict(self.expectations), dict(self.calibration)
                require_rows = list(gate.MUTATIONS)
                bound, unaffected = copy.deepcopy(gate.BOUND), copy.deepcopy(gate.UNAFFECTED)
                if extended:
                    expectations['listing-probe'] = 'EXPECTATION_LISTING_PROBE'
                    expectations['listing-derivation-probe'] = 'EXPECTATION_RUST_DERIVATION'
                    calibration['listing-probe'] = 'CALIBRATION_LISTING_PROBE'
                    require_rows.append(('listing-probe', gate.SESSION, 'old', 'new'))
                    bound['listing-probe'] = (
                        ('listing-probe', 'EXPECTATION_LISTING_PROBE'),
                        ('listing-derivation-probe', 'EXPECTATION_RUST_DERIVATION'),
                    )
                    unaffected['listing-probe'] = ('listing-derivation-probe',
                                                  'EXPECTATION_RUST_DERIVATION')
                tables = {'EXPECTATION_CASES': expectations, 'CALIBRATION_CASES': calibration}
                with patch.object(sys, 'argv', ['negative_controls.py', '--list']), \
                        patch.object(gate, 'panel_table', side_effect=tables.__getitem__), \
                        patch.object(gate, 'MUTATIONS', require_rows), \
                        patch.object(gate, 'BOUND', bound), patch.object(gate, 'UNAFFECTED', unaffected), \
                        contextlib.redirect_stdout(io.StringIO()) as output:
                    self.assertTrue(gate.main() == 0, '#1190 list: valid tables must pass')
                try:
                    listing = json.loads(output.getvalue())
                except ValueError as error:
                    self.fail(f'#1190 list: expected JSON: {error}')
                self.assertTrue(listing.get('EXPECTATION_CASES') == expectations,
                                '#1190 list: expectation names and rules must come from the live table')
                self.assertTrue(listing.get('CALIBRATION_CASES') == calibration,
                                '#1190 list: calibration names and rules must come from the live table')
                self.assertTrue(listing.get('MUTATIONS') == [row[0] for row in require_rows],
                                '#1190 list: mutation names must come from the live table')
                self.assertTrue(listing.get('counts', {}).get('derivationExpectationCases')
                                == (10 if extended else 9),
                                '#1190 list: derivation probe must raise count from 9 to 10')
                self.assertTrue(listing.get('counts') == {
                    'expectationCases': len(expectations),
                    'derivationExpectationCases': sum(rule == 'EXPECTATION_RUST_DERIVATION'
                                                     for rule in expectations.values()),
                    'calibrationCases': len(calibration), 'mutationRows': len(require_rows)},
                    '#1190 list: counts must be derived from the live tables')

    def test_list_rejects_invalid_only(self):
        known = gate.MUTATIONS[0][0]
        for names in (['bogus'], [known, known]):
            with self.subTest(names=names), \
                    patch.object(sys, 'argv', ['negative_controls.py', '--list'] +
                                 [arg for name in names for arg in ('--only', name)]), \
                    contextlib.redirect_stdout(io.StringIO()) as output, \
                    contextlib.redirect_stderr(io.StringIO()) as errors:
                self.assertTrue(gate.main() == 2,
                                '#1190 list-only: invalid selection must reject listing')
                self.assertTrue('unknown or duplicate --only name' in errors.getvalue()
                                and not output.getvalue(),
                                '#1190 list-only: diagnostic must precede listing')

    def test_list_runs_anchor_checks(self):
        selected = gate.MUTATIONS[:1]
        with patch.object(sys, 'argv', ['negative_controls.py', '--list', '--check-anchors',
                                      '--only', selected[0][0]]), \
                _pinned_anchor_reads(self.lock_events, self.lock), \
                patch.object(gate, 'check_anchors', wraps=gate.check_anchors) as anchors, \
                contextlib.redirect_stdout(io.StringIO()) as output:
            self.assertTrue(gate.main() == 0,
                            '#1190 list-anchors: valid anchors and listing must pass')
        self.assertTrue(anchors.call_count == 1 and anchors.call_args.args == (selected,),
                        '#1190 list-anchors: selected anchors must execute before listing')
        unlocked = [event for event in self.lock_events
                    if isinstance(event, tuple) and event[0] == 'unlocked-read']
        self.assertTrue(not unlocked and 'lock-enter' in self.lock_events,
                        '#1190 list-anchors: the anchor read must happen inside '
                        f'the tree lock window: {self.lock_events!r}')
        report, separator, payload = output.getvalue().partition('\n')
        self.assertTrue(report == '1 anchors: ok' and bool(separator),
                        '#1190 list-anchors: anchor report must precede listing')
        try:
            listing = json.loads(payload)
        except ValueError as error:
            self.fail(f'#1190 list-anchors: JSON listing must follow anchor report: {error}')
        self.assertTrue(listing.get('MUTATIONS') == [row[0] for row in gate.MUTATIONS],
                        '#1190 list-anchors: combined invocation must also list live tables')

    def test_list_rejects_failed_anchors(self):
        with patch.object(sys, 'argv', ['negative_controls.py', '--list', '--check-anchors']), \
                patch.object(gate, 'check_anchors', return_value=2) as anchors, \
                contextlib.redirect_stdout(io.StringIO()) as output:
            self.assertTrue(gate.main() == 2,
                            '#1190 list-anchors: failed anchors must reject listing')
        self.assertTrue(anchors.call_count == 1 and not output.getvalue(),
                        '#1190 list-anchors: failed anchors must not print listing')

    def test_list_is_read_only(self):
        active = False

        def audit(event, args):
            if not active:
                return
            if event == 'open':
                _path, mode, flags = args
                writable = (isinstance(mode, str) and any(char in mode for char in 'wax+')) or (
                    isinstance(flags, int) and flags & (os.O_WRONLY | os.O_RDWR | os.O_CREAT |
                                                       os.O_TRUNC | os.O_APPEND))
                if not writable:
                    return
            elif event not in {'os.mkdir', 'os.rename', 'os.remove', 'os.rmdir',
                               'os.symlink', 'os.link', 'os.truncate', 'os.chmod',
                               'os.chown', 'os.utime', 'subprocess.Popen', 'os.system'}:
                return
            raise AssertionError(f'#1190 list: filesystem writes forbidden: {event}')

        sys.addaudithook(audit)
        with patch.object(sys, 'argv', ['negative_controls.py', '--list',
                                      '--output', '/unused', '--source', '/unused']), \
                contextlib.redirect_stdout(io.StringIO()):
            try:
                active = True
                self.assertTrue(gate.main() == 0, '#1190 list: read-only invocation must pass')
            finally:
                active = False

    def test_list_rejects_binding_drift(self):
        bound = copy.deepcopy(gate.BOUND)
        bound['drop-registry-state-require'] = (('expectations-rust-registry-state', 'WRONG'),)
        with patch.object(sys, 'argv', ['negative_controls.py', '--list']), \
                patch.object(gate, 'BOUND', bound), \
                contextlib.redirect_stdout(io.StringIO()) as output, \
                contextlib.redirect_stderr(io.StringIO()) as errors:
            self.assertTrue(gate.main() == 2, '#1190 list: binding drift must reject CLI')
        self.assertTrue('BINDING DRIFT:' in errors.getvalue() and not output.getvalue(),
                        '#1190 list: binding diagnostic must precede listing')

    def test_standalone_check_anchors(self):
        active = False

        def audit(event, args):
            if not active:
                return
            if event == 'open':
                _path, mode, flags = args
                writable = (isinstance(mode, str) and any(char in mode for char in 'wax+')) or (
                    isinstance(flags, int) and flags & (os.O_WRONLY | os.O_RDWR | os.O_CREAT |
                                                       os.O_TRUNC | os.O_APPEND))
                if not writable:
                    return
            elif event not in {'os.mkdir', 'os.rename', 'os.remove', 'os.rmdir',
                               'os.symlink', 'os.link', 'os.truncate', 'os.chmod',
                               'os.chown', 'os.utime', 'subprocess.Popen', 'os.system'}:
                return
            raise AssertionError(f'#1279 standalone-anchors: filesystem writes forbidden: {event}')

        sys.addaudithook(audit)
        # #2795 fix-r4（评审建议①）：真 TreeLock 的 os.open(O_RDWR|O_CREAT) 会被
        # 上面的审计钩子判写（实测红：filesystem writes forbidden: open）；setUp
        # 已换成 _RecordingTreeLock 记录桩，只验「读面在锁内」。
        with patch.object(sys, 'argv', ['negative_controls.py', '--check-anchors']), \
                _pinned_anchor_reads(self.lock_events, self.lock), \
                patch.object(gate, 'check_anchors', wraps=gate.check_anchors) as anchors, \
                contextlib.redirect_stdout(io.StringIO()) as output, \
                contextlib.redirect_stderr(io.StringIO()) as errors:
            try:
                active = True
                self.assertTrue(gate.main() == 0,
                                '#1279 standalone-anchors: rc must be 0 without --output/--source')
            finally:
                active = False
        unlocked = [event for event in self.lock_events
                    if isinstance(event, tuple) and event[0] == 'unlocked-read']
        self.assertTrue(not unlocked,
                        '#2795 every anchor read must happen with the tree lock '
                        f'held: {unlocked!r}')
        self.assertTrue(self.lock_events
                        and self.lock_events[0] == 'lock-enter'
                        and self.lock_events[-1] == 'lock-exit',
                        f'#2795 the lock must wrap the whole anchor loop: '
                        f'{self.lock_events!r}')
        # 两次顺序窗口（panel_table 预检 + check_anchors），无嵌套；
        # 每次都必须锁 fixture 根 + 60 s 有界等待。
        self.assertTrue(len(self.lock.calls) >= 2
                        and all(call == ((gate.ROOT,), {'timeout': 60})
                                for call in self.lock.calls),
                        f'#2795 the anchor preflight must lock the fixture root '
                        f'with the 60 s bounded wait: {self.lock.calls!r}')
        self.assertGreater(len(gate.MUTATIONS), 0,
                           '#1364 empty-anchors: the shipped mutation table must not be empty')
        self.assertTrue(anchors.call_count == 1
                        and list(anchors.call_args.args[0]) == list(gate.MUTATIONS),
                        '#1279 standalone-anchors: the full mutation table must be preflighted, '
                        'rows compared whole (name, source, old, new) — not just the names')
        # #1364 P0-b: set equality with an empty table and "0 anchors: ok" both
        # used to pass, so the self-check certified zero validated anchors.
        self.assertGreater(len(anchors.call_args.args[0]), 0,
                           '#1364 empty-anchors: zero validated anchors must not count as ok')
        lines = output.getvalue().splitlines()
        self.assertTrue(lines, '#1279 standalone-anchors: the anchor report must be printed')
        self.assertTrue(lines[-1] == f'{len(gate.MUTATIONS)} anchors: ok',
                        '#1279 standalone-anchors: the anchor report must be the final output line')
        self.assertRegex(lines[-1], r'^[1-9][0-9]* anchors: ok$',
                         '#1364 empty-anchors: the report must name a positive anchor count, '
                         'never "0 anchors: ok"')
        self.assertTrue(not any(line.lstrip().startswith('{') for line in lines),
                        '#1279 standalone-anchors: standalone must not print the JSON listing')
        self.assertTrue(errors.getvalue() == '',
                        '#1279 standalone-anchors: the ok path must not print usage or drift diagnostics')
        with patch.object(sys, 'argv', ['negative_controls.py', '--check-anchors']), \
                patch.object(gate, 'check_anchors', return_value=2), \
                contextlib.redirect_stdout(io.StringIO()), \
                contextlib.redirect_stderr(io.StringIO()):
            self.assertTrue(gate.main() == 2,
                            '#1279 standalone-anchors: failed anchors must fail standalone too')

    def test_check_anchors_rejects_empty_selection(self):
        """#1364 P0-b: zero anchors must fail closed, never print "0 anchors: ok"."""
        with contextlib.redirect_stdout(io.StringIO()) as output, \
                contextlib.redirect_stderr(io.StringIO()) as errors:
            self.assertEqual(gate.check_anchors([]), 2,
                             '#1364 empty-anchors: an empty selection must not return success')
        self.assertTrue('EMPTY ANCHOR SELECTION' in errors.getvalue(),
                        '#1364 empty-anchors: the failure must name its cause on stderr')
        self.assertTrue(output.getvalue() == '',
                        '#1364 empty-anchors: an empty selection must not report an ok count')

    def test_standalone_check_anchors_rejects_empty_table(self):
        """#1364 P0-b: the CLI preflight must fail when MUTATIONS is empty."""
        with patch.object(sys, 'argv', ['negative_controls.py', '--check-anchors']), \
                patch.object(gate, 'MUTATIONS', []), \
                patch.object(gate, 'BOUND', {}), \
                patch.object(gate, 'UNAFFECTED', {}), \
                patch.object(gate, 'check_bindings', return_value=[]), \
                contextlib.redirect_stdout(io.StringIO()) as output, \
                contextlib.redirect_stderr(io.StringIO()) as errors:
            self.assertTrue(gate.main() == 2,
                            '#1364 empty-anchors: an empty mutation table must fail the preflight')
        self.assertTrue('EMPTY ANCHOR SELECTION' in errors.getvalue(),
                        '#1364 empty-anchors: the preflight failure must name its cause')
        self.assertTrue(not any(line.endswith('anchors: ok')
                               for line in output.getvalue().splitlines()),
                        '#1364 empty-anchors: no invocation may report an ok anchor count')

    def test_early_errors_have_fresh_evidence_and_continue(self):
        source = Mock()
        rows = [('first', source, 'absent', ''), ('second', source, 'pass', 'invalid !')]
        recorded = []
        with patch.object(sys, 'argv', ['negative_controls.py', '--output', '/unused', '--source', '/unused']), \
                patch.object(gate, 'MUTATIONS', rows), \
                patch.object(gate, 'BOUND', {name: (('case', 'rule'),) for name, *_ in rows}), \
                patch.object(gate, 'UNAFFECTED', {name: ('case', 'rule') for name, *_ in rows}), \
                patch.object(gate, 'check_bindings', return_value=[]), \
                patch.object(gate, 'new_output_directory', return_value=MagicMock()), \
                patch.object(gate, 'mutating_sources', return_value=contextlib.nullcontext({source: b'pass\n'})), \
                patch.object(gate, 'publish_summary', side_effect=lambda _out, data: recorded.extend(data)), \
                contextlib.redirect_stdout(io.StringIO()):
            try:
                code = gate.main()
            except Exception as error:
                self.fail(f'#1190 evidence: early errors escaped: {type(error).__name__}: {error}')
            self.assertTrue(code == 1, '#1190 evidence: early errors must stay row-local')
        self.assertTrue(len(recorded) == 2, '#1190 evidence: must continue after early error')
        expected = {'baseline': [], 'mutant': [], 'unaffected': None, 'restored': None}
        self.assertTrue(all(row['evidence'] == expected for row in recorded),
                        '#1190 evidence: early errors need initialized empty evidence')
        self.assertTrue(recorded[0]['evidence'] is not recorded[1]['evidence'],
                        '#1190 evidence: never carry previous row evidence')
        self.assertTrue('anchor expired' in recorded[0]['reason'] and 'SyntaxError' in recorded[1]['reason'],
                        '#1190 evidence: exercise anchor and compile failures')


class SetupClassReadSurface(unittest.TestCase):
    """#2795 fix-r5（评审 concurrency P2#1 升格）：setUpClass 读面的机检。

    fix-r4 给 main() 的预检（panel_table / check_anchors）补了树锁，但
    setUpClass 早于 setUp 的记录桩，两处 panel_table 读仍在锁外。本类直接
    驱动真实的 setUpClass：只把 negative_process.tree_lock 换成记录桩
    （装置内部经 negative_process 全局查 tree_lock），读面记名判红；
    busy 时装置必须 SystemExit(2) + 统一文案（与名册驱动的拒跑形态同形）。
    fix-r6 补第三钉（评审 concurrency#2）：--negative 元控入口经
    _meta_class_setup 复用记录桩，不依赖真锁空闲；真入口保真锁
    （锁忙 = SystemExit(2) fail-closed，实测 unittest 对 setUpClass 的
    SystemExit 不出报告、整进程退出）。
    """

    def test_setup_class_reads_panels_inside_the_tree_lock(self):
        events = []
        lock = _RecordingTreeLock(events)
        with patch.object(negative_process, 'tree_lock', lock), \
                _pinned_anchor_reads(events, lock):
            DriftTests.setUpClass()
        unlocked = [event for event in events
                    if isinstance(event, tuple) and event[0] == 'unlocked-read']
        self.assertTrue(not unlocked and events[:1] == ['lock-enter']
                        and events[-1:] == ['lock-exit'],
                        '#2795 meta-controls: setUpClass must read the panel '
                        f'source inside the shared anchor_read_window: {events!r}')
        self.assertTrue(lock.calls == [((gate.ROOT,), {'timeout': 60})],
                        '#2795 meta-controls: setUpClass must lock the repo '
                        f'root with the 60 s bounded wait: {lock.calls!r}')

    def test_setup_class_refuses_when_the_tree_lock_is_busy(self):
        events = []
        lock = _RecordingTreeLock(events, busy=True)
        errors = io.StringIO()
        with patch.object(negative_process, 'tree_lock', lock), \
                contextlib.redirect_stderr(errors):
            with self.assertRaises(SystemExit) as caught:
                DriftTests.setUpClass()
        self.assertTrue(caught.exception.code == 2,
                        '#2795 meta-controls: a busy tree lock must refuse '
                        f'setUpClass with rc 2, got {caught.exception.code!r}')
        self.assertTrue(
            'session-domain-expectations meta-controls: '
            'tree lock or anchor read failed' in errors.getvalue()
            and 'TimeoutError' in errors.getvalue(),
            '#2795 meta-controls: the refusal must use the shared device '
            f'message and name the lock wait: {errors.getvalue()!r}')
        self.assertTrue(events == [],
                        '#2795 meta-controls: a refused setUpClass must read '
                        f'no panel bytes: {events!r}')

    def test_negative_entry_does_not_depend_on_the_real_lock(self):
        """#2795 fix-r6（评审 concurrency#2）：--negative 元控入口的锁 seam。

        真入口（unittest.main）保真锁；但 ``--negative`` 跑整套变体，不应依赖
        ``.negative-source.lock`` 空闲（setUp 换记录桩的同一理由）。真锁忙时
        （外层 busy 桩模拟兄弟持锁）元控的类建面必须照常完成——若
        ``_meta_class_setup`` 退回直接调 setUpClass（真锁），busy 桩会
        SystemExit(2)，本用例红。
        """
        busy = []
        with patch.object(negative_process, 'tree_lock',
                          _RecordingTreeLock(busy, busy=True)):
            events = _meta_class_setup()
        self.assertTrue(events[:1] == ['lock-enter'] and events[-1:] == ['lock-exit']
                        and 'unlocked-read' not in events,
                        '#2795 meta-controls: the --negative entry must run '
                        f'its class setup through the stub seam even with a '
                        f'busy real lock: {events!r}')
        self.assertTrue(busy == [],
                        '#2795 meta-controls: the busy real lock must never '
                        f'be reached by the meta entry: {busy!r}')


def _meta_class_setup():
    """The ``--negative`` meta entry's class setup: the recording-stub seam.

    #2795 fix-r6（评审 concurrency#2）：真入口（unittest.main）的 setUpClass
    保真锁——锁忙时装置 SystemExit(2) 是既定 fail-closed（实测确认 SystemExit
    穿过 unittest._handleClassSetUp、整进程退出、无报告）；但 ``--negative``
    元控跑整套 18 个变体，不必也不该依赖 ``.negative-source.lock`` 空闲
    （setUp 换记录桩的同一理由）。这里在调用 setUpClass 期间把装置内部的
    ``negative_process.tree_lock`` 换成记录桩——装置行为本身不变，只有锁
    的实现被替换。返回桩记录的事件（机检：
    test_negative_entry_does_not_depend_on_the_real_lock）。
    """
    events = []
    with patch.object(negative_process, 'tree_lock', _RecordingTreeLock(events)):
        DriftTests.setUpClass()
    return events


def negative_checks():
    """Compile each function variant, run the bound test, restore, then summarize all."""
    variants = (
        ('skip-binding-checks', 'check_bindings', '    errors = []', '    return []',
         'test_binding_corruptions_are_rejected',
         '#1190 bindings: corruption must have its bound diagnostic'),
        ('skip-registered-rule', 'check_bindings', 'if registered != rule:', 'if False:',
         'test_wrong_rule_binding_is_rejected',
         '#1190 wrong-rule: registered rule mismatch must be rejected'),
        ('omit-expectation-list', 'main', "'EXPECTATION_CASES': expectation_cases",
         "'EXPECTATION_CASES': {}", 'test_list_reports_live_tables_and_counts',
         '#1190 list: expectation names and rules must come from the live table'),
        ('omit-calibration-list', 'main', "'CALIBRATION_CASES': calibration_cases",
         "'CALIBRATION_CASES': {}", 'test_list_reports_live_tables_and_counts',
         '#1190 list: calibration names and rules must come from the live table'),
        ('omit-mutation-list', 'main', "'MUTATIONS': [row[0] for row in MUTATIONS]",
         "'MUTATIONS': []", 'test_list_reports_live_tables_and_counts',
         '#1190 list: mutation names must come from the live table'),
        ('wrong-mutation-count', 'derived_counts', 'len(mutations)', 'len(mutations) + 1',
         'test_list_reports_live_tables_and_counts',
         '#1190 list: counts must be derived from the live tables'),
        ('constant-derivation-count', 'derived_counts',
         "sum(rule == 'EXPECTATION_RUST_DERIVATION'\n"
         "                    for rule in expectation_cases.values())", '9',
         'test_list_reports_live_tables_and_counts',
         '#1190 list: derivation probe must raise count from 9 to 10'),
        ('skip-only-validation', 'main',
         'if args.only and (set(args.only) - known or len(args.only) != len(set(args.only))):',
         'if False:', 'test_list_rejects_invalid_only',
         '#1190 list-only: invalid selection must reject listing'),
        ('skip-list-anchors', 'main', 'if args.check_anchors:',
         'if args.check_anchors and not args.list:', 'test_list_runs_anchor_checks',
         '#1190 list-anchors: selected anchors must execute before listing'),
        ('return-before-listing', 'main', 'if anchor_status != 0 or not args.list:',
         'if True:', 'test_list_runs_anchor_checks',
         '#1190 list-anchors: JSON listing must follow anchor report'),
        ('ignore-anchor-failure', 'main', 'if anchor_status != 0 or not args.list:',
         'if not args.list:', 'test_list_rejects_failed_anchors',
         '#1190 list-anchors: failed anchors must reject listing'),
        ('drop-standalone-return', 'main', 'if anchor_status != 0 or not args.list:',
         'if anchor_status != 0:', 'test_standalone_check_anchors',
         '#1279 standalone-anchors: rc must be 0 without --output/--source'),
        ('write-during-list', 'main', '    if args.list:',
         "    if args.list:\n        sys.audit('os.mkdir', '/unused', 511, -1)",
         'test_list_is_read_only', '#1190 list: filesystem writes forbidden'),
        ('skip-cli-bindings', 'main',
         'check_bindings(expectation_cases, MUTATIONS, BOUND, UNAFFECTED)', '[]',
         'test_list_rejects_binding_drift', '#1190 list: binding drift must reject CLI'),
        ('late-evidence', 'main',
         "                evidence = {'baseline': [], 'mutant': [], 'unaffected': None,\n"
         "                            'restored': None}\n", '',
         'test_early_errors_have_fresh_evidence_and_continue',
         '#1190 evidence: early errors must stay row-local'),
        # #1364 P0-b: dropping the empty-selection guard restores the old
        # "0 anchors: ok" / rc 0 behaviour, which the bound test must catch.
        ('empty-anchors-pass', 'check_anchors',
         "    if not selected:\n"
         "        print('EMPTY ANCHOR SELECTION: nothing to validate; '\n"
         "              'the anchor gate must not pass', file=sys.stderr)\n"
         "        return 2\n", '',
         'test_check_anchors_rejects_empty_selection',
         '#1364 empty-anchors: an empty selection must not return success'),
    )
    _meta_class_setup()
    results = []
    for name, function, before, after, method, marker in variants:
        row = {'mutation': name, 'test': f'test_meta_controls.DriftTests.{method}',
               'assertion': marker, 'status': 'INVALID', 'compiled': False}
        try:
            module = gate
            text = inspect.getsource(getattr(module, function))
            if text.count(before) != 1:
                raise ValueError(f'#1190 {name}: stale in-memory anchor')
            namespace = {}
            exec(compile(text.replace(before, after, 1), f'<#1190 {name}>', 'exec'),
                 module.__dict__, namespace)
            changed = namespace[function]
            row['compiled'] = True
            positive = unittest.TestResult()
            DriftTests(method).run(positive)
            if not positive.wasSuccessful():
                raise ValueError('#1190 positive failed: ' + str(positive.errors + positive.failures))
            negative = unittest.TestResult()
            with patch.object(module, function, changed), contextlib.redirect_stdout(io.StringIO()):
                DriftTests(method).run(negative)
            failures = negative.errors + negative.failures
            row['raw'] = [detail for _test, detail in failures]
            if negative.wasSuccessful():
                row.update(status='SURVIVED', reason='named test passed under variant')
            elif (not negative.errors and negative.failures and
                  all(any('AssertionError' in line and marker in line for line in detail.splitlines())
                      for _test, detail in negative.failures)):
                row.update(status='KILLED', reason='named test failed with its exclusive assertion')
            else:
                row['reason'] = 'unrelated exception or missing bound assertion'
            restored = unittest.TestResult()
            DriftTests(method).run(restored)
            if not restored.wasSuccessful():
                raise ValueError('#1190 restored test failed')
        except Exception as error:
            row.update(status='INVALID', reason=f'{type(error).__name__}: {error}')
        results.append(row)
    counts = {state: sum(row['status'] == state for row in results)
              for state in ('KILLED', 'SURVIVED', 'INVALID')}
    print(json.dumps({'controls': results, 'counts': counts}, indent=2))
    return 0 if all(row['status'] == 'KILLED' for row in results) else 1


if __name__ == '__main__':
    if sys.argv[1:] == ['--negative']:
        raise SystemExit(negative_checks())
    unittest.main(verbosity=2)
